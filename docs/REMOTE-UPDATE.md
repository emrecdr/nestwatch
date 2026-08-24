# Updating the PC without walking to it

Nestwatch has no auto-update, and deliberately so — see [Why there is no updater](#why-there-is-no-updater)
at the end. What follows is how to install a new build over the network instead, and how to do it
without handing your child a way in.

**Read the prerequisite first.** Most of this guide is worse than useless if you skip it.

---

## Before anything: your child must not be a local administrator

Run this on the PC:

```powershell
Get-LocalGroupMember -SID S-1-5-32-544 | Select-Object Name
```

If the account your child signs in with is listed, **stop here.**

Not as a precaution — as a matter of arithmetic. Everything below opens an administrative way
into that machine. A child who is already an administrator can use it himself, and he does not
need to guess a password to do it: he *is* the administrator. He can also stop Nestwatch, delete
its service, and read its data folder, so remote updates are the least of what changes.

Fix it from an account that will remain an administrator:

```powershell
Remove-LocalGroupMember -Group "Administrators" -Member "<his-account>"
```

Make sure you can still sign in as an administrator afterwards — the built-in `Administrator`
account with a password you know, or your own account on that PC. Then re-run the check above and
confirm he is gone.

`nestwatch doctor` lists the administrators on every run, near the bottom.

---

## Is this worth setting up at all?

Honest answer: often not.

Releases are occasional. Enabling remote management is permanent, and it adds an entry point to a
machine whose whole design assumes the person using it is trying to get around you. Walking to the
PC once every few months costs less than most people expect, and costs nothing in risk.

Set this up if visiting is genuinely impractical. If it is merely inconvenient, don't.

---

## The method: PowerShell Remoting over HTTPS

Three things make this the right choice here rather than the usual advice:

- **Your laptop connects *in*.** The monitored PC still contacts nothing, which is the promise the
  rest of this project makes.
- **It does not disturb the screen.** Unlike Remote Desktop, which takes over the console session,
  a remoting session is invisible to whoever is sitting at the PC.
- **It runs on Windows Home**, not just Pro. Remote Desktop's *server* side does not.

### Why HTTPS and not the default

`Enable-PSRemoting` sets up an **HTTP** listener on port 5985, and every quick tutorial then tells
you to add the machine to `TrustedHosts`. On a home network — which is a *workgroup*, not a
domain — that combination is the wrong answer:

- Without a domain there is no Kerberos, so authentication falls back to **NTLM**, and NTLM does
  not prove the server's identity. Microsoft's own guidance is blunt about it: computers in the
  `TrustedHosts` list *might not be authenticated*.
- Over WinRM HTTP, someone on the same network can **passively capture the NetNTLMv2 exchange and
  crack it offline.** On this network, "someone on the same network" is the person you are trying
  to manage, sitting at a PC with plenty of time.

So: HTTPS, with a certificate your laptop actually trusts. Then the machine is authenticated, the
handshake is encrypted, and no password material is exposed to anyone listening.

---

## Setting it up

Steps 1–4 run **on the child's PC** in an elevated PowerShell. Steps 5–6 run **on your laptop.**

### 1. Turn on remoting

```powershell
Enable-PSRemoting -Force
```

### 2. Make a certificate for it

The name must match exactly what you will type when connecting.

```powershell
$cert = New-SelfSignedCertificate `
    -DnsName $env:COMPUTERNAME `
    -CertStoreLocation Cert:\LocalMachine\My
$cert.Thumbprint
```

Write that thumbprint down. You will check it from the other side.

### 3. Listen on HTTPS, and stop listening on HTTP

```powershell
New-Item -Path WSMan:\localhost\Listener -Transport HTTPS -Address * `
         -CertificateThumbPrint $cert.Thumbprint -Force

# Remove the plaintext listener Enable-PSRemoting created. Leaving it is the whole
# problem described above -- an attacker simply connects to 5985 instead.
winrm delete winrm/config/Listener?Address=*+Transport=HTTP
```

Confirm only HTTPS remains:

```powershell
winrm enumerate winrm/config/Listener
```

### 4. Let only the home network reach it

```powershell
# Allow HTTPS remoting from this subnet only, on Private networks only.
New-NetFirewallRule -DisplayName "WinRM HTTPS (LAN only)" `
    -Direction Inbound -Protocol TCP -LocalPort 5986 `
    -RemoteAddress LocalSubnet -Profile Private -Action Allow

# And make sure the plaintext port stays shut. Matched on the port number, not on a
# rule name: Windows localises those, and the built-in one is "Windows Remote
# Management (HTTP-In)" only on English installs. (Nestwatch's own code avoids the
# same trap by looking groups up by SID rather than by the name "Administrators".)
Get-NetFirewallRule | Where-Object {
    ($_ | Get-NetFirewallPortFilter).LocalPort -eq 5985
} | Set-NetFirewallRule -Enabled False
```

Narrow it further to just your laptop if its address is fixed: replace `LocalSubnet` with that
address.

Export the certificate so your laptop can trust it:

```powershell
Export-Certificate -Cert $cert -FilePath C:\Users\Public\winrm.cer
```

Copy that file to your laptop — a USB stick, or any method you would trust with it.

### 5. Trust the certificate, on your laptop

Elevated PowerShell:

```powershell
# Check the thumbprint matches the one you wrote down in step 2 BEFORE importing.
(Get-PfxCertificate -FilePath .\winrm.cer).Thumbprint

Import-Certificate -FilePath .\winrm.cer -CertStoreLocation Cert:\LocalMachine\Root
```

Comparing that thumbprint is the step that makes the rest of this safe. It is the same reasoning
as the TLS fingerprint Nestwatch prints at install: a self-signed certificate is only as good as
the one time you check it came from the right machine.

**Never use `-SkipCACheck` or `-SkipCNCheck`.** They switch off exactly the protection this whole
section exists to provide, and every "just get it working" answer online recommends them.

### 6. Connect

```powershell
$s = New-PSSession -ComputerName CAMDERE-WINDOWS -UseSSL -Credential (Get-Credential)
```

Enter an **administrator** account on the child's PC — not his account. If this fails with a
certificate error, the name you typed does not match the certificate, or the import did not take.
Fix that rather than reaching for a `-Skip` flag.

---

## Doing an update

```powershell
# 1. On your laptop: get the release and check it before it goes anywhere.
#    See the release page for both commands.
Get-FileHash nestwatch.exe -Algorithm SHA256 | Format-List
gh attestation verify nestwatch.exe --repo emrecdr/nestwatch

# 2. Copy it over the session you opened above.
Copy-Item .\nestwatch.exe -Destination 'C:\Windows\Temp\nestwatch.exe' -ToSession $s

# 3. Install. This stops the service, replaces the binary, and starts it again.
Invoke-Command -Session $s { C:\Windows\Temp\nestwatch.exe install }

# 4. Confirm what is now running.
Invoke-Command -Session $s { & 'C:\Program Files\HostHealth\host-health.exe' version }
Invoke-Command -Session $s { & 'C:\Program Files\HostHealth\host-health.exe' doctor }

# 5. Tidy up.
Invoke-Command -Session $s { Remove-Item C:\Windows\Temp\nestwatch.exe -Force }
Remove-PSSession $s
```

Verify the download **before** copying it, on the machine where you can read the output. That is
the only point in this sequence where a tampered binary can still be caught.

`install` prompts for a password and runs its pre-flight checks the same as it does locally, so
watch what it prints rather than assuming it worked. Enforcement is off for the few seconds
between stop and start.

---

## Turning it off again

There is no reason to leave this listening between updates.

```powershell
Disable-PSRemoting -Force
Get-NetFirewallRule -DisplayName "WinRM HTTPS (LAN only)" | Set-NetFirewallRule -Enabled False
Stop-Service WinRM
Set-Service WinRM -StartupType Manual
```

That rule name is one you created in step 4, so it is not localised and matching it by name is
safe.

Re-enable when you next need it. `Disable-PSRemoting` does not remove the listener or the firewall
rule, which is why the second line is there.

---

## Why there is no updater

Nestwatch will not fetch or install its own updates, and the reason is not effort.

**It would have to phone home.** A version check from the service means the monitored PC contacting
a server, which ends "nothing leaves the house" and leaks your address and roughly when a child's
computer is awake. The dashboard's *"check for a newer version"* button avoids this by running in
your browser, on your device — the PC itself still contacts nothing.

**Self-updating services are a well-documented way to lose a machine.** The service runs as SYSTEM.
An updater is, by construction, a path that writes an executable and runs it with full privileges —
and the record on exactly that component class is poor. In the last year, in Microsoft's own update
components: an unauthenticated remote code execution as SYSTEM in WSUS (CVE-2025-59287, serious
enough for a CISA out-of-band alert), plus several local privilege-escalation flaws. More than one
is worded as *an authorised local attacker elevates privileges* — which describes your child on
that PC precisely.

Doing it safely means verifying a signature before execution, in-process, in security-critical
code, on a platform where this project's behaviour is still only partly verified. The cost is real
and the benefit is saving a few minutes every few months.

**And it would not stay hidden anyway.** Any update stops and restarts the service and replaces a
file in `Program Files`, both visible to anyone looking. Nestwatch is meant to be overt.

---

## What not to do

| Don't | Why |
|---|---|
| `Enable-PSRemoting` and leave HTTP on | NTLM over HTTP on a workgroup LAN: the exchange can be captured and cracked offline by anyone on the network, including your child. |
| `Set-Item WSMan:\localhost\Client\TrustedHosts -Value *` | Turns off server authentication for every host you connect to, everywhere. |
| `-SkipCACheck` / `-SkipCNCheck` | Discards the certificate check, leaving encryption with nothing behind it. |
| Enable remoting while your child is an administrator | He can use it himself, and does not need to guess anything. |
| Leave remoting on between updates | A permanent entry point for something you use twice a year. |
| Remote Desktop instead | Takes over the console session, so he sees it — and the server side is not available on Windows Home. |

---

## Status of this guide

The reasoning here is verified: the NTLM and `TrustedHosts` weaknesses are Microsoft's own
documented guidance, PowerShell Remoting is confirmed supported on Windows Home, and it is
confirmed not to disconnect the console session the way Remote Desktop does.

The **command sequences have not been run on the target machine.** They are Windows-only and
belong to the same tier as everything in [WINDOWS-TESTING.md](WINDOWS-TESTING.md) — the tier where
every serious problem this project has had was found. Work through the setup once with the PC in
front of you, confirm each step prints what it should, and only rely on it remotely after that.
