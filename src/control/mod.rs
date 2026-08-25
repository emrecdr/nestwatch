//! The OS abstraction boundary.
//!
//! Everything the app can *do* to the machine goes through [`SystemControl`]. The web
//! layer depends only on this trait, never on `xcap`/`sysinfo`/`shutdown` directly, so:
//!   * the real Windows behaviour is quarantined in `windows.rs`,
//!   * a deterministic [`FakeControl`] lets the whole server build and be tested on macOS,
//!   * new capabilities (e.g. live streaming) can be added without touching handlers.
//!
//! Methods are **synchronous** on purpose: they wrap blocking OS calls. Handlers invoke
//! them via `tokio::task::spawn_blocking` so the async runtime is never stalled, and the
//! trait stays `dyn`-compatible without needing `async-trait`.

use std::sync::Arc;

use serde::Serialize;

mod fake;
#[cfg(windows)]
mod service_control;
#[cfg(windows)]
mod windows;

pub use fake::FakeControl;

/// A single running process, as surfaced to the dashboard.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    /// Resident memory in bytes (rendered human-readably in the UI).
    pub memory_bytes: u64,
}

/// A running process as the **enforcement tick** needs to see it: what it is, and how to stop it.
///
/// Deliberately narrower than [`ProcessInfo`], and a separate type rather than that one with a
/// field left at zero. The two are gathered at very different costs, and the type is what keeps
/// them apart.
///
/// [`SystemControl::list_processes`] renders a panel a parent opens occasionally, so it can afford
/// a memory figure. This one runs every `CHECK_INTERVAL` for the life of the machine and reads
/// exactly these two fields. A single type carrying an unused `memory_bytes` is how the cheap scan
/// came to be paying for the expensive number in the first place — leaving the field present, even
/// zeroed, invites that back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcess {
    pub pid: u32,
    pub name: String,
}

/// Whether an interactive user is present at the console — drives screen-time accounting so the
/// budget isn't charged while nobody is using the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// No user is logged in at the console (e.g. the machine is at the sign-in screen, or off).
    /// Nothing to charge time to.
    NoUser,
    /// A user is logged in but the workstation is locked (lock screen / screensaver). Present,
    /// but not actively using the machine.
    Locked,
    /// A user is logged in and the session is unlocked — actively usable.
    Active,
}

/// How much of the screen a capture is expected to carry.
///
/// The axis is **intent, not resolution**: which tier applies is decided by whether a person asked
/// or a timer did, and the parent never chooses a picture size. The dashboard already encodes that
/// distinction in its markup — the *Take screenshot* and modal *Refresh* buttons are people, the
/// live interval is a clock — so this adds no control to the UI.
///
/// The split exists because the two are not the same job and do not cost the same. Measured through
/// this crate's encoder on a 4K desktop showing a game: native PNG was **20,641 KiB**, and the same
/// frame at [`Preview`](ShotTier::Preview) is about **23 KiB**. More usefully, the preview figure
/// barely moves — 23–32 KiB across every content type and source resolution tried — while the
/// native one varies **132×** on content nobody controls. A predictable cost is what makes this a
/// tier rather than a gamble.
///
/// One variant, one code path, one parameter — deliberately not two methods. Separate
/// `capture_preview()` / `capture_full()` functions would give the preview path all the exercise
/// and let the full path rot, and the full path is the one used at the tense moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotTier {
    /// Ambient view, fed by the live timer. Fitted inside [`PREVIEW_W`]×[`PREVIEW_H`] and encoded
    /// at [`PREVIEW_QUALITY`]. Sized about 1.4× the dashboard card's real display area, so it stays
    /// sharp at a 1.5× device pixel ratio without paying for pixels the card discards.
    Preview,
    /// A deliberate look, fed by a person pressing something. Native resolution at
    /// [`FULL_QUALITY`], because this is the tier a parent uses to actually *read* something and
    /// JPEG rings around small text at low quality.
    Full,
}

impl ShotTier {
    /// The tier's name on the wire — the `?tier=` query the dashboard sends and the `--tier`
    /// argument the service passes to the helper.
    ///
    /// One definition for both, on purpose. They are the same value crossing two different
    /// boundaries, and a second spelling of "preview" in either place is a mismatch that would show
    /// up as a silently full-resolution live stream: no error, no failing test, just the cost back.
    pub fn as_arg(self) -> &'static str {
        match self {
            ShotTier::Preview => "preview",
            ShotTier::Full => "full",
        }
    }

    /// Parse a tier from the wire. **Unknown and absent both mean [`Full`](ShotTier::Full)** —
    /// the tier that behaves as this endpoint always has. A typo therefore costs bandwidth, which
    /// is visible and recoverable; the opposite default would quietly hand a parent a blurry
    /// picture at the moment they asked for a sharp one.
    pub fn from_arg(s: Option<&str>) -> Self {
        match s {
            Some("preview") => ShotTier::Preview,
            _ => ShotTier::Full,
        }
    }
}

/// Longest edge of a preview frame. See [`ShotTier::Preview`] for why this size.
pub(crate) const PREVIEW_W: u32 = 960;
/// Tallest a preview frame gets. Paired with [`PREVIEW_W`] as a bounding box, not a forced aspect —
/// the capture is fitted inside it so a 16:10 or rotated monitor is never stretched.
pub(crate) const PREVIEW_H: u32 = 540;
/// JPEG quality for [`ShotTier::Preview`]. Ample for "what kind of thing is on screen".
const PREVIEW_QUALITY: u8 = 70;
/// JPEG quality for [`ShotTier::Full`]. Higher because this tier exists to make text legible.
const FULL_QUALITY: u8 = 90;

/// The MIME type every capture is returned as, named once for the handler that stamps the header
/// and the trait doc that promises it. `tests/api.rs` asserts the literal `"image/jpeg"` rather
/// than this constant on purpose: a wire test comparing the response against the same constant the
/// response was built from would pass just as happily if the constant and the encoder disagreed.
pub(crate) const SHOT_MIME: &str = "image/jpeg";

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("no process with pid {0}")]
    ProcessNotFound(u32),

    #[error("screen capture failed: {0}")]
    Capture(String),

    #[error("operation failed: {0}")]
    Op(String),
}

/// The set of remote operations the server can perform on the host machine.
pub trait SystemControl: Send + Sync + 'static {
    /// Capture the primary monitor at `tier` and return JPEG-encoded bytes ([`SHOT_MIME`]).
    ///
    /// Implementations must apply the tier **before** the bytes leave the process that captured
    /// them. On Windows that process is a helper in the child's session and the result crosses a
    /// pipe, where the difference is the whole point: the same 4K frame is 32,400 KiB as raw RGBA,
    /// 20,641 KiB as PNG, and **47 KiB** resized and encoded first. Downscaling on the service side
    /// would save the parent's bandwidth and none of the cost that actually matters.
    fn screenshot(&self, tier: ShotTier) -> Result<Vec<u8>, ControlError>;

    /// List currently running processes with their memory use, for the dashboard's process panel.
    /// Called on demand, when a parent opens that card.
    fn list_processes(&self) -> Result<Vec<ProcessInfo>, ControlError>;

    /// The pid and name of every running process — the enforcement tick's view, and nothing more.
    ///
    /// Split from [`list_processes`](Self::list_processes) because the two run on completely
    /// different schedules and the difference is not free. On Windows the memory figure costs a
    /// kernel call per process, and the default refresh that used to serve both callers also
    /// computed CPU percentage, disk-I/O counters and the full executable path for every process
    /// on the machine — several hundred of them, twice a minute, forever, all discarded here.
    ///
    /// Ordering is unspecified: the enforcer folds the result into a set.
    fn running_processes(&self) -> Result<Vec<RunningProcess>, ControlError>;

    /// Terminate the process with the given PID.
    fn kill_process(&self, pid: u32) -> Result<(), ControlError>;

    /// Lock the interactive session (require the user's password to resume). Softer than a
    /// shutdown: nothing is powered off and no work is lost.
    fn lock_workstation(&self) -> Result<(), ControlError>;

    /// Begin an orderly shutdown of the machine after `delay_secs`, optionally showing the
    /// user a warning `message` during the countdown.
    fn shutdown(&self, delay_secs: u32, message: Option<String>) -> Result<(), ControlError>;

    /// Cancel a shutdown previously scheduled by [`SystemControl::shutdown`]. Idempotent:
    /// succeeds even if none is pending. Used by the curfew enforcer to undo a countdown
    /// when the window ends or curfew is disabled.
    fn abort_shutdown(&self) -> Result<(), ControlError>;

    /// Report whether an interactive user is present and actively using the console session.
    /// The screen-time enforcer consults this so it doesn't charge the daily budget while
    /// nobody is logged in or the screen is locked. Best-effort: the enforcer treats an `Err`
    /// as [`SessionState::Active`] — failing toward enforcement, never toward unlimited time.
    fn session_state(&self) -> Result<SessionState, ControlError>;

    /// Show the interactive user a brief notification — used to warn the child before a
    /// screen-time lock, or when a Warn-mode limit is reached, so enforcement isn't a silent
    /// surprise. Best-effort and **non-blocking**: it returns immediately (the message
    /// auto-dismisses) and never waits for the user to click.
    fn notify_user(&self, title: String, body: String) -> Result<(), ControlError>;
}

/// Show the interactive user a notification from an async context, off the runtime.
///
/// Lives here rather than in either enforcer because both need it: the rules enforcer warns
/// before a screen-time lock, curfew before bedtime. (The trait method stays synchronous like
/// every other one — this is just the `spawn_blocking` wrapper around it.)
///
/// Returns **whether the OS accepted the message**, so callers can record warnings that were
/// actually delivered rather than ones that were merely intended. That distinction is the same
/// one the enforcers already make for locks and shutdowns: a notification can fail for reasons
/// nobody would otherwise see (a stale console session id, fast-user-switching, no interactive
/// user), and a countdown that silently never reaches the child looks identical in the logs to
/// one that did. Failure is never fatal — a missed warning must not stall or crash an enforcer.
pub async fn notify(control: &Arc<dyn SystemControl>, title: &str, body: &str) -> bool {
    let control = control.clone();
    let (title, body) = (title.to_string(), body.to_string());
    match tokio::task::spawn_blocking(move || control.notify_user(title, body)).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "child notification failed");
            false
        }
        Err(e) => {
            tracing::error!(error = %e, "child notification task panicked");
            false
        }
    }
}

/// Fit `img` to `tier` and encode it as JPEG. Shared by the real and fake controllers so the
/// sizing, quality and error-mapping live in one place (child modules see this private helper).
///
/// **The alpha channel never reaches the encoder, and arranging that costs nothing.** A desktop
/// capture is opaque by construction, so a quarter of every byte the old PNG path fed its encoder
/// was the constant 255. Measured on a 4K frame with the compression level held fixed, dropping it
/// alone was 10% smaller and 15% faster. JPEG has no alpha channel at all, so the encoder discards
/// it without being asked — see the note on the `match` below for why converting first was waste.
///
/// `Triangle` for the resample: `Nearest` shimmers on text as the window moves and `Lanczos3` costs
/// several times as much for a frame the parent glances at.
fn encode_shot(img: image::DynamicImage, tier: ShotTier) -> Result<Vec<u8>, ControlError> {
    let (fitted, quality) = match tier {
        // Only ever **down**. `DynamicImage::resize` fits the image *to* the box in both
        // directions, so a 640x360 frame would be scaled up to 960x540 — spending bytes to invent
        // detail that is not there. Worse, it would disguise the defect
        // `SetProcessDpiAwarenessContext` exists to fix: a DPI-virtualised capture arrives
        // undersized, and stretching it back out makes a broken capture look merely soft. Caught by
        // `a_small_frame_is_never_scaled_up`, which failed on exactly this.
        ShotTier::Preview if img.width() > PREVIEW_W || img.height() > PREVIEW_H => (
            img.resize(PREVIEW_W, PREVIEW_H, image::imageops::FilterType::Triangle),
            PREVIEW_QUALITY,
        ),
        ShotTier::Preview => (img, PREVIEW_QUALITY),
        ShotTier::Full => (img, FULL_QUALITY),
    };

    let mut out = Vec::new();
    {
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        // Hand the encoder the buffer that already exists, in whatever shape it arrived.
        //
        // There used to be a `to_rgb8()` here, and it bought nothing: `image`'s JPEG encoder sends
        // `Rgba8` and `Rgb8` down the same `encode_rgb`, whose `rgb_to_ycbcr` reads three channels
        // and ignores any fourth. So the conversion allocated and filled a second copy of the frame
        // — 23.7 MiB on a 4K capture, 1.48 MiB on *every* preview frame — to hand the encoder bytes
        // it would have derived itself.
        //
        // Checked rather than reasoned about, by encoding one frame both ways and diffing the
        // output. The test frame carried a deliberately **varying** alpha channel, so a leak of any
        // kind — premultiplication, a fourth plane — would have changed the bytes. It did not:
        // byte-identical at q70 and q90, and a shade faster (44.1 ms against 45.1 at 4K).
        //
        // The `match` is load-bearing, not tidiness. Passing `&fitted` straight in compiles and
        // looks cleaner, but `DynamicImage`'s own `GenericImageView` re-matches the enum on every
        // single `get_pixel`: measured at 48.1 ms, slower than the copy this removes. Naming the
        // variant hands the encoder a flat slice instead.
        match fitted {
            image::DynamicImage::ImageRgba8(buf) => enc.encode_image(&buf),
            // Everything else, the fake controller's `Rgb8` included. `into_` rather than `to_`, so
            // a frame already in this shape is moved rather than cloned.
            other => enc.encode_image(&other.into_rgb8()),
        }
        .map_err(|e| ControlError::Capture(e.to_string()))?;
    }
    Ok(out)
}

/// Controller for an **interactive** process (dev `run`, or the session helper): captures
/// the screen directly. On non-Windows this is the fake.
pub fn interactive_control() -> Arc<dyn SystemControl> {
    #[cfg(windows)]
    {
        Arc::new(windows::WindowsControl::new())
    }
    #[cfg(not(windows))]
    {
        Arc::new(FakeControl::new())
    }
}

/// Controller for the **SYSTEM service** (Session 0): process/kill/shutdown run directly,
/// but screenshots are delegated to a helper launched into the interactive session, since
/// Session 0 has no desktop to capture. On non-Windows this is the fake.
pub fn service_control() -> Arc<dyn SystemControl> {
    #[cfg(windows)]
    {
        Arc::new(service_control::ServiceControl::new())
    }
    #[cfg(not(windows))]
    {
        Arc::new(FakeControl::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The capture backend must be **named** in `Cargo.toml`, never left to a default.
    ///
    /// This is the guard for a defect that produced no warning, no error and no failing test, and
    /// survived thirteen review passes as a result. `xcap` declares **no `default` feature list**,
    /// so `xcap = "0.9"` silently selected its `#[cfg(not(feature = "wgc"))]` arm — GDI `BitBlt`,
    /// which returns **black** for exclusive-fullscreen games and DRM video. A child can select
    /// that failure from a game's own display settings.
    ///
    /// Reading the manifest as text, rather than testing a capture, is deliberate: the wrong
    /// backend is only observable at runtime on Windows, in front of specific content, on hardware
    /// this project has never had. The manifest is checkable everywhere, including from the
    /// machine this is developed on, and it is where the mistake is actually made.
    ///
    /// It fires on the next `cargo upgrade` too — if xcap ever gains a `default` list, this still
    /// insists the choice be written down rather than inherited.
    #[test]
    fn the_capture_backend_is_named_not_defaulted() {
        let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        let line = manifest
            .lines()
            .find(|l| l.trim_start().starts_with("xcap"))
            .expect("Cargo.toml must declare xcap");
        assert!(
            line.contains("\"wgc\""),
            "xcap must name its capture backend. This line leaves it to a default that does not \
             exist, which silently selects the GDI path and captures fullscreen games as black:\n  \
             {line}"
        );
    }

    /// The preview tier must actually be smaller, at the one place both tiers are produced.
    ///
    /// `tests/api.rs` covers this end to end through the HTTP handler; this covers `encode_shot`
    /// itself, so a regression is attributed to the encoder rather than to the route.
    #[test]
    fn a_preview_is_smaller_than_the_frame_it_came_from() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(1920, 1080, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        }));
        let full = encode_shot(img.clone(), ShotTier::Full).expect("full encodes");
        let preview = encode_shot(img, ShotTier::Preview).expect("preview encodes");
        assert!(
            preview.len() * 2 < full.len(),
            "preview {} B vs full {} B — the tier is not being applied",
            preview.len(),
            full.len()
        );
    }

    /// The arm that actually ships is the one no CI machine runs.
    ///
    /// `control/windows.rs` always hands `encode_shot` an `ImageRgba8`, and every controller a test
    /// can reach is the fake, which produces `Rgb8` — so the RGBA arm of the encoder `match` has no
    /// coverage at all on Linux or macOS. This supplies it, and pins the property that let the
    /// `to_rgb8()` call be deleted: JPEG carries no alpha channel, so an opaque frame and its RGB
    /// equivalent must encode to the same bytes. If a future edit reintroduces a conversion, or
    /// swaps the arms, this still passes — it is here for the case where the RGBA arm stops
    /// producing a correct picture, which is invisible from any other test on this platform.
    #[test]
    fn a_frame_with_alpha_encodes_exactly_as_the_same_frame_without_it() {
        let px = |x: u32, y: u32| [(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8];
        let rgb = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(320, 200, |x, y| {
            image::Rgb(px(x, y))
        }));
        // Opaque, as a desktop capture always is.
        let rgba = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(320, 200, |x, y| {
            let [r, g, b] = px(x, y);
            image::Rgba([r, g, b, 255])
        }));

        for tier in [ShotTier::Preview, ShotTier::Full] {
            let from_rgb = encode_shot(rgb.clone(), tier).expect("rgb encodes");
            let from_rgba = encode_shot(rgba.clone(), tier).expect("rgba encodes");
            assert_eq!(
                from_rgb,
                from_rgba,
                "{} tier: {} B from Rgb8 against {} B from Rgba8 — the alpha channel is reaching \
                 the encoder, so the capture path and the fake are no longer producing the same \
                 picture",
                tier.as_arg(),
                from_rgb.len(),
                from_rgba.len()
            );
            assert!(!from_rgba.is_empty(), "an empty JPEG is not a picture");
        }
    }

    /// A frame already smaller than the preview box is not scaled **up**.
    ///
    /// Upscaling would cost bytes to invent detail, and would hide the very defect
    /// `SetProcessDpiAwarenessContext` exists to fix: a DPI-virtualised capture arrives small, and
    /// blowing it back up to 960x540 would make a broken capture look merely soft.
    #[test]
    fn a_small_frame_is_never_scaled_up() {
        let small = image::DynamicImage::ImageRgb8(image::RgbImage::new(640, 360));
        let preview = encode_shot(small, ShotTier::Preview).expect("encodes");
        // Assert the DIMENSION, because that is the rule. This used to compare byte counts against
        // a full-tier encode, which held mostly because q70 < q90 on an all-black fixture — a
        // future fixture (textured, or larger) could upscale and still satisfy it.
        let decoded = image::load_from_memory(&preview).expect("a preview is a decodable image");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (640, 360),
            "a 640x360 frame must come back at its own size, not stretched to the preview box"
        );
    }

    /// Round-trip every tier through the wire spelling. A mismatch here would show up as a live
    /// stream silently running at full resolution — no error, no failing route, just the cost back.
    #[test]
    fn every_tier_survives_the_wire_spelling() {
        for tier in [ShotTier::Preview, ShotTier::Full] {
            assert_eq!(ShotTier::from_arg(Some(tier.as_arg())), tier);
        }
    }

    /// Absent and unrecognised both mean `Full` — the tier this endpoint has always returned.
    #[test]
    fn an_unknown_tier_is_full_not_preview() {
        for input in [
            None,
            Some(""),
            Some("PREVIEW"),
            Some("thumbnail"),
            Some("0"),
        ] {
            assert_eq!(
                ShotTier::from_arg(input),
                ShotTier::Full,
                "{input:?} must not be read as a preview"
            );
        }
    }
}
