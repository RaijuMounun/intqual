# Spec: target validation and IPv6

Status: done 2026-11-05 (commits `fa40b5e`, `6ea70f8`). Roadmap items H1–H5.

## Problem

A full MCP test on Windows 11 found the following:

- An empty target pings the local machine.
- Malformed targets such as `"  google.com  "`, `https://google.com`, `google.com; whoami` and
  300-character names reach DNS and come back as a misleading `dns_resolution` error.
- IPv6 does not work at all: `::1` gives a `socket` error on every sample, because the Windows
  provider accepts only IPv4 (`ipv4_target` in `icmp/windows.rs`) and the Unix providers open
  `Domain::IPV4` sockets.
- On a dual-stack network, a hostname that resolves to an IPv6 address first fails over ICMP
  but works over TCP.

## Goal

1. Every entry point (TUI, headless, MCP) validates and normalises the target the same way,
   before any DNS lookup. A bad target gives a clear error that names the problem.
2. Ping and traceroute work over IPv6 on Windows, without admin.
3. The user can force the address family: `-4` and `-6` on the CLI, `ip_version` in MCP.

## Approach

### 1. Target validation (core)

There is one pure function in core, used by `measure` and by the TUI start path in `main.rs`:
`parse_target(&str) -> Result<Target, ProbeError>`.

Step by step, it:

- Trims surrounding whitespace.
- Rejects an empty string.
- Accepts an IPv4 literal, an IPv6 literal (`::1`, `2606:4700::1111`), and an IPv6 literal in
  brackets (`[::1]`, the brackets are stripped).
  - IPv6 zone ids (`fe80::1%12`) are accepted on Windows, where `%12` is the interface index.
- Rejects a URL, meaning anything with `://`, with a hint: "give a hostname or IP address, not a
  URL (try `google.com`)".
- Rejects `host:port`, with a hint to use the port option.
- Otherwise checks the value as a hostname under RFC 1123 / RFC 1035:
  - total length ≤ 253 characters, labels 1–63 characters;
  - letters, digits and `-` only, no `-` at the start or end of a label;
  - one trailing dot is allowed;
  - no spaces or shell characters.
  - Internationalised names are out of scope; they are rejected with a "use the punycode
    (`xn--`) form" hint.
- Returns `Target::Ip(IpAddr)` or `Target::Host(String)`. An IP literal skips DNS.

Each entry point reports a failure in its own way:

- **New `ProbeError` variant:** `InvalidTarget(String)` with `kind()` `invalid_target`.
- **CLI:** exit code 2 ("could not start"). The error goes to stderr, or into the JSON report
  as `error.kind = "invalid_target"`, the same way `dns_resolution` already is.
- **MCP:** an `isError` result `{"kind":"invalid_target","message":...}`, so the model can fix
  the target itself.
- **TUI:** the existing startup error path, before the alternate screen opens.

### 2. Address family

- After DNS, the code picks one address and uses it for every probe in the run (ICMP, TCP and
  traceroute), so ICMP and TCP never disagree.
- **Default (`auto`):** the first address in OS order. `getaddrinfo` already sorts by RFC 6724,
  so this matches what `ping` and browsers do.
- **`-4` / `-6` (CLI)** and **`ip_version: "auto" | "v4" | "v6"` (MCP, default `auto`):**
  take the first address of that family.
  - If there is none, the error is the new `ProbeError::NoAddressForFamily`, kind
    `no_address_for_family`, with a message such as "google.com has no IPv6 address".
  - An IP literal of the other family gives the same error.
- The chosen address is already in the report as `resolved_ip`. Add `ip_version: "v4" | "v6"`
  to the report. This is a new field, so `schema_version` stays 1. Adding a field is a
  compatible change; update the golden test.

### 3. ICMPv6 on Windows

`icmp/windows.rs` gets a v6 path next to the v4 one, behind the same `IcmpProvider` trait:

- `Icmp6CreateFile` opens the handle, and `Icmp6SendEcho2` sends the echo with a
  `sockaddr_in6` source (unspecified, `::`) and destination.
- Replies are parsed with `Icmp6ParseReplies`, giving `ICMPV6_ECHO_REPLY_LH`.
- The TTL for traceroute is set through `IP_OPTION_INFORMATION.Ttl`, the same field the v4 path
  uses; ICMPv6 calls it the hop limit.
- Status codes are mapped through a pure `map_status`-style function with a table test, as on
  v4. The status names are the same.
- There is one provider type, and it picks the v4 or v6 API by the target's family. Ping and
  traceroute code does not change.
- Every `unsafe` block gets a `// SAFETY:` comment.

### 4. Unix

Unix is out of scope (Windows first). A v6 target there returns the new
`ProbeError::Unsupported(String)` (kind `unsupported`) with the text "IPv6 ICMP is not yet
supported on this platform", instead of a socket error. TCP over IPv6 already works on every
OS. This code is not compiled on Windows: say so, and let CI check it.

### 5. Small fixes (H4, H5)

- **Traceroute budget error:** it names `max_hops` and `timeout_ms` instead of "count,
  interval".
- **Argument error messages:** a single prefix, for example `invalid arguments: missing field
  'target'`, not "Invalid request: invalid arguments: …".
- **Traceroute report `count`:** it keeps `count: 1`, for schema stability. Document in the MCP
  spec that traceroute ignores it.

## Tests

- **Unit, `parse_target`:** a table test covering at least:
  - the empty string, whitespace only, and surrounding whitespace;
  - an IPv4 literal, IPv6 literals in normal and bracketed form, and a zone id;
  - a URL, `host:port`, a shell character, a space inside;
  - labels of length 63/64, a total length of 253/254, a leading or trailing `-`;
  - one trailing dot, two dots, a non-ASCII name.

  Assert the exact `ProbeError` variant and the hint text.
- **Unit, address family:** a pure `pick_address(&[IpAddr], IpVersion)` covers auto, v4 and
  v6, including no match and an IP literal of the other family.
- **Unit, Windows v6 status mapping:** a table test.
- **Loopback, `#[cfg(windows)]`:** ping `::1` succeeds through the real provider, and
  traceroute to `::1` ends at hop 1 with `complete: true`. Both run without admin.
- **MCP:**
  - `target: ""` and `target: "https://x.com"` give `invalid_target`;
  - `ip_version: "v6"` with `--mock` gives `ip_version: "v6"` in the report;
  - the traceroute budget message names `max_hops`.
- **CLI binary:** `intqual -c 1 --json ""` exits 2 with `invalid_target`, and `-c 1 -6 ::1`
  succeeds.
- **Manual (for the user):** ping and traceroute to `2606:4700:4700::1111` and
  `ipv6.google.com`, and `-6 google.com`. This needs an IPv6-capable connection; if the ISP has
  no IPv6, expect a clean ICMP error, not a hang.

## Out of scope

- Unix ICMPv6 (roadmap, Later).
- Happy Eyeballs (RFC 8305), which races v4 and v6. A measurement tool measures one address on
  purpose.
- Internationalised domain names: punycode conversion is not done.

## Done when

- [x] H1–H5 are fixed, and each MCP test case from the roadmap gives the expected new result.
- [x] `::1` ping and traceroute work on Windows without admin.
- [x] The fmt, build, test and clippy checks pass, and the test review has run.
- [x] The roadmap is updated with status `done` and the commit.

## Implementation notes (deviations)

- `pick_address` takes `&[SocketAddr]` (keeps the IPv6 scope id) and an `auto_allows_ipv6`
  flag. On Unix, where ICMPv6 is not written yet, `auto` keeps preferring IPv4, so a
  dual-stack host does not turn every ICMP sample into `unsupported`. On Windows `auto` is
  the first address in OS order, as specified.
- `Target::Ip` carries a `scope_id`. Zone ids must be numeric (`fe80::1%12`) on every OS.
- `InvalidRequest` now displays its message without the "Invalid request:" prefix (H5); the
  three new variants do the same. Clients match on `kind`.
- With `--mock`, `ip_version: v6` reports `2001:db8::1`; the mock traceroute keeps its IPv4
  hops.
- The Unix TUI traceroute worker (sudo child) does not get `-4`/`-6` passed on; it resolves
  with `auto`. Unix IPv6 is deferred anyway.
- Windows: synchronous `Icmp6SendEcho2` (no event, no APC), source `::`, then
  `Icmp6ParseReplies`; ping/traceroute error types use ICMPv6 numbers (unreachable 1,
  time exceeded 3).
