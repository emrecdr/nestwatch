// Thin binary entry point. All real logic lives in the library crate so it can be
// exercised by integration tests in `tests/`.
fn main() -> anyhow::Result<()> {
    nestwatch::run_cli()
}
