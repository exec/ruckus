# Porting status

Ruckus is an early Rust client port of Lurk with a **GPUI desktop frontend**.
It is not yet a replacement for Lurk. This document distinguishes reusable
library support from an end-to-end desktop feature.

| Lurk area | Rust status | Remaining desktop/parity work |
|---|---|---|
| `irc` | Parse/serialize, IRCv3 tags, numerics/commands, validation and byte budgets | Broader differential/fuzz corpus; non-UTF-8 server text policy |
| `conn` | Bounded TCP/verified TLS, deadlines, cancellation-safe reads and fail-closed writes | Proxy support and custom/client certificate policy |
| `cap` | LS 302 continuation, ACK/NAK, NEW/DEL/LIST, SASL registration hold | Public capability customization/status UI |
| `isupport` | Token merges/removal, casemapping, PREFIX, CHANMODES, typed limits | More server-dialect compatibility tests |
| `sasl` | PLAIN/EXTERNAL helpers; required PLAIN integrated into the client | SCRAM-SHA-256, client certificates/EXTERNAL end-to-end, Unicode preparation |
| `client` | Background registration, PING/PONG, bounded events/commands, member/topic snapshots | Auto-reconnect/rejoin, richer semantic events, CTCP auto-replies, MONITOR state |
| Actions | Validated command builders and relay-budgeted UTF-8 PRIVMSG/NOTICE/ACTION splitting | Integrate all commands into desktop menus/composer |
| Member state | JOIN/PART/QUIT/NICK/KICK, NAMES reconciliation, prefix MODE, topics, ACCOUNT/AWAY/CHGHOST | Arbitrary channel mode state; WHO/WHOIS result models |
| History | Nested bounded BATCH tracker and CHATHISTORY builders | Client/frontend response integration, paging, deduplication, reconnect recovery |
| `config` | JSON model, XDG paths, validation/edit helpers, atomic restrictive Unix writes | Saved-network launcher and credential UX; migration/import policy |
| `chatlog` | Safe bounded append helpers and restrictive Unix permissions | Desktop settings/wiring, retention and date presentation |
| Frontend | GPUI connection form, conversation sidebar, chat, member list/topic, composer and basic slash commands | Multi-network sessions, per-buffer drafts/unread/highlights, completion, search, formatting, notifications, richer commands |
| Typing/standard replies | Raw tags and messages preserved | Typing timers/send throttle and dedicated FAIL/WARN/NOTE presentation |
| Bouncer integration | Generic IRC connection is possible | soju network binding/discovery/management helpers and UI |
| `lurkd` server/backlog | Not in the client-port scope | Keep using the Go daemon |
| Rust TUI | Not implemented | Optional later frontend using the same core |

## Intended next slices

1. Exercise the real desktop connection/composer workflow on each supported
   platform; expand the synthetic GPUI input/focus/layout tests and visual checks.
2. Wire saved-network configuration and logging into desktop sessions without
   weakening TLS/authentication policy.
3. Add reconnect lifecycle and replay/history semantics before adding multi-network
   recovery UI.
4. Work through Lurk's IRCv3 support matrix capability by capability, testing both
   library state and observable frontend behavior.

Validation must remain hermetic. Passing unit tests is not evidence that all of
Lurk's live behavior has been reproduced; unverified platform/UI workflows must
be reported explicitly.
