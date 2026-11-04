# GlobalProtect protocol notes (from official client binaries)

Reverse-engineering recon of the official Palo Alto GlobalProtect installers
(`win_mac_binaries/`, client version **5.2.13-48**, Feb 2023). These notes
confirm the wire behavior OpenConnect already implements and give us the
official client's exact strings/params to mimic where it matters.

Analyzed with [REA](https://github.com/morluto/rea) (v6.8.0, provider-free
static inspections: `inspect-pe-imports`, `inspect-pe-certificates`) plus
manual extraction with 7z and string sweeps. Deep decompilation was not run
(no Hopper/Ghidra/IDA provider installed) — everything below is from headers,
imports, certificates, and string tables, so it is evidence-backed but not a
full behavioral reconstruction.

## Component map (Windows x64 MSI)

| CAB stream | File | Role |
|---|---|---|
| `_FFF9FE8F…` | PanGPS.exe (~7.5 MB) | The VPN service: portal/gateway HTTP, tunnel, HIP |
| `_3258F9B4…` | PanGPA.exe (~5.7 MB) | UI agent: SAML WebBrowser control, notifications, PLAP |
| `_9A0EFD50…` / `_486111A4…` | PanPUAC.exe (10.7 / 0.7 MB) | Elevated helper (UAC) |
| `_368DA7BA…` | GlobalProtect.exe | Launcher |
| `PanGPA_*.dll` ×6 | — | UI language packs |
| `_F0DF3114…` | OLE blob (112 MB) | Merge modules (drivers, vcredist, etc.) |
| `_D97DD14F…` ×3 | DER X.509 | Embedded signing/intermediate certs |
| macOS pkg | `GlobalProtect` 5.2.13-48 | Mach-O UI/service bundle + `PanGpHipMp`, `gpsplit-helper` |

### REA import/certificate findings (evidence)

- `PanGPS.exe` (x86_64, pe32-plus, sha256 `b028fecf…`, Authenticode PKCS#7
  signed through DigiCert Assured ID Root CA — DigiCert SHA2 Assured ID Code
  Signing CA) imports **LIBEAY32/SSLEAY32** (OpenSSL 1.0.x-era names) for TLS,
  **WINHTTP/WININET** for portal HTTP, **fwpuclnt.dll** (Windows Filtering
  Platform — its local kill-switch/filtering), **PsvCtrl.dll** (Palo Alto
  service control IPC), plus IPHLPAPI/SETUPAPI (interface management).
- `PanGPA.exe` (sha256 `d727a277…`) imports **urlmon** (the embedded IE
  WebBrowser SAML view), **ncrypt/WinSCard** (machine cert / smart card auth),
  and talks to PanGPS over local IPC.
- Conclusion for us: the official client's TLS, tunnel, and WFP filtering are
  all replaceable — OpenConnect provides TLS+ESP; a kill-switch feature would
  map to WFP on Windows (future work, see README).

## HTTP surface

Endpoints (`.esp` = "external server page", POST form-encoded unless noted):

- `/global-protect/prelogin.esp` — portal login handshake; returns
  `prelogin-response` XML with `auth-api` (auth method list), authentication
  modifiers, and for SAML the `saml-prelogin-portal` marker.
- `/global-protect/getconfig.esp` — portal config; returns `getconfig[1].xml`
  with `gateway-list` / `gateways-v6`, `portal-config-version`, DNS/split-tunnel
  rules, `client-upgrade` directive (`global-protect-full` / `global-protect-lite`).
- `/global-protect/getmsi.esp` — client software download/upgrade.
- `/ssl-tunnel-connect.sslvpn` (GET, params below) — gateway tunnel setup over TLS.
- `/global-protect/hipreport.esp`, `hipreportcheck.esp` — host posture report
  submission/check.

Key request params observed:

- Gateway connect query:
  `user=%s&portal=%s&authcookie=%s&domain=%s&computer=%s&os-version=%s&clientos=%s&os-type=…&platform=windows|mac&panos-version=…&app-version=…&clientVer=…&jnlpReady=…`
- Cookie names: `portal-userauthcookie`, `portal-prelogonuserauthcookie`,
  `prelogin-cookie`, `userauthcookie`, plus `gateway-userauthcookie` family.
- Prelogon ("user and machine") tunnels exist: `PreLogonState` registry key,
  machine-certificate auth.

## Identification strings

- User-Agent: `PAN GlobalProtect/%s (%s)` — e.g.
  `Microsoft Windows 10 Enterprise 10.0.17763` as the OS string; macOS uses
  `Mac OS X` for `clientos`/`os-type`.
- `platform=windows` / `platform=mac`.
- Registry/config root: `SOFTWARE\Palo Alto Networks\GlobalProtect\PanSetup`.

## Behavior worth mimicking in GPConnect

- SAML on Windows is done **inside the client** via an embedded IE
  WebBrowser control (`CPanSAMLView`), watching `saml-auth-status` values
  `-1/0/1` and the final username; cookies are cleared after. Our approach
  (OpenConnect `--external-browser`) is equivalent but opens the user's real
  browser — keep it, it is the modern replacement for exactly this control.
- Error taxonomy: `auth-failed-password-expired`, `auth-failed-invalid-cookie`,
  `invalid-gateway-credential`, `gateway-otp-prompt` (OTP at gateway),
  `portal-csc-auth-cookie`, `portal-certificate-verification` … map these to
  ConnState::Error messages instead of dumping raw openconnect text where
  possible.
- Gateway selection: portal returns a `gateway-list`; client picks by
  `gateway-location`/manual choice. OpenConnect does this internally; expose
  the chosen gateway in the UI.
- Upgrade channel: `client-upgrade` in getconfig can force-upgrade the
  official client; an OSS client should surface "portal requires client
  upgrade" as an info message rather than auto-installing.

## Practical conclusion

Nothing here contradicts the OpenConnect-based architecture — the official
client's HTTP vocabulary (`prelogin.esp` → `getconfig.esp` →
`ssl-tunnel-connect.sslvpn` → `hipreport.esp`) is exactly what OpenConnect's
GP implementation speaks. Keep wrapping OpenConnect; use these strings as the
reference for error mapping and for matching portal expectations
(`platform`/`clientos` knobs can be passed through if a portal filters on
them — openconnect supports `--os`, `--user-agent` not needed since GP
hardcodes its own).
