# Asynchronous routing on simoom `rt-same-loop` (2026-11-20, WSL2)

These are the raw results for `docs/design/async-routing.md`, section 13.

**Setup.** The method is that of `../2026-11-19-wsl-async-proxy/`:
- `proxy.sh` and the same CPU pinning;
- three rounds, with the variants interleaved;
- each run started only when Windows was below 15% CPU.

**Builds:**
- `phase0`: Cameleer at `2165d00` on the released simoom 0.1.0. `mode=sync` is the same binary with
  `CAMEL_ASYNC_ROUTING=false`.
- `rsl`: the same Cameleer source built against simoom branch `rt-same-loop` (`460aeaf`, not
  released).
  - `build-rsl.sh` points every simoom crate at an export of that branch with `--config
    patch.crates-io...`. No Cargo file is changed.
  - On that branch, `RtClient::send` called on a simoom loop sends on that loop, and a loop's own
    replies go out in the same turn.

`proxy-any.sh` picks the build (`fix` is the code of section 14's fixes on the same simoom). `rounds-rsl.out`
and `rounds-fix.out` hold the raw lines.

Profile (section 14):
- `profile-windows.txt`: `cameleer-bench profile http-proxy 8 60` on Windows 11, built against the
  same simoom export with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`.
- `build-prof.sh` and `prof.sh`: the `perf` version for WSL. It needs `kernel.perf_event_paranoid`
  at 1 or lower, which the owner sets.
