
---

## Verify before you run it

This runs as SYSTEM on the machine you install it on. Two checks, and the second is the stronger:

```powershell
Get-FileHash nestwatch.exe -Algorithm SHA256 | Format-List   # compare with nestwatch.exe.sha256
gh attestation verify nestwatch.exe --repo emrecdr/nestwatch
```

The checksum only proves the download did not corrupt — it is published by this same workflow, so
anyone able to publish a release could publish a matching hash. The attestation is a signature
proving this exact binary was built by this repository's release workflow, from commit
`__COMMIT__`. It fails closed: the lookup is keyed on the file's own digest, so a modified binary
is not a signature that fails to match, it is a file that was never signed.

Then right-click the .exe → Properties → tick **Unblock** before installing.

**[Install guide](https://emrecdr.github.io/nestwatch/)** · [Security model](https://github.com/emrecdr/nestwatch/blob/main/docs/SECURITY.md) · [Known limits](https://github.com/emrecdr/nestwatch/blob/main/docs/OPEN-FINDINGS.md)
