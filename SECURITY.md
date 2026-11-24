# Security Policy

## Supported Versions

Security fixes go into the latest release. Upgrade with `cargo install electron-to-rust`
before reporting, in case the issue is already fixed.

| Version | Supported |
| --- | --- |
| Latest minor (0.1.x) | Yes |
| Older | No |

## Reporting a Vulnerability

Please report vulnerabilities privately, not in a public issue. Use
[Report a vulnerability](https://github.com/mr-vishwet/rust-made-easy/security/advisories/new)
on the repository's Security tab.

Include what you found, how to reproduce it, which version and platform you used, and what an
attacker could do with it. You'll get a reply within a few days, and you'll hear when a fix is
out. If you'd like credit in the release notes, say how you want to be named.

## Scope

`e2r` is a local tool with a specific trust profile, and it helps to state it plainly:

- **It reads your code.** `scan` and `convert` parse the Electron app you point them at. They
  never send it anywhere: the tool makes no network requests, phones home never, and collects
  no telemetry. The only network use is yours — `cargo` fetching crates when you build the
  generated project.
- **It generates code.** `convert` writes a Tauri 2 project to the directory you choose. That
  generated code is yours to read before you build it, and you should read it — especially the
  parts flagged ASSISTED or MANUAL in `MIGRATION-NOTES.md`.
- **It does not execute your app.** `scan` and `convert` are static: they read files, they don't
  run them. (A future record/replay phase will run apps deliberately, and will say so loudly
  when it does.)

These are in scope:

- Generated code with memory-unsafe behavior, or that does something the mapping didn't ask
  for — for example a generated command that reads or writes files outside the app's own data
  directories.
- A crafted Electron project (a malicious `package.json`, `main.js`, or preload script) that
  makes `e2r` write outside the `--out` directory, execute a command, or leak data.
- A path-traversal or symlink escape in scan or convert: the tool must only read inside the
  app directory you named and only write inside the output directory you named.
- The release pipeline: the published crates.io crate not matching the tagged source.

These are not vulnerabilities on their own:

- `e2r` faithfully converting an Electron app that itself does something dangerous. The tool
  mirrors your app's behavior; auditing your app is your job, and `MIGRATION-NOTES.md` flags
  the spots worth a second look.
- Running `e2r` on an untrusted repository and then building and running the generated project
  without reading it. The generated code is code: read it before you run it, as you would any
  code generator's output.

## Disclosure

Once a fix is released, the advisory is published with credit to the reporter, unless they
prefer otherwise.
