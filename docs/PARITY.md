# Parity

We measure **behavioural parity** with the original game, not byte-matching builds. A matching build, a
deterministic export and behavioural parity are separate numbers and are never added together.

| Area | Measure | Oracle | Status |
|---|---|---|---|
| Package reading | Every shipped package parses; offsets/sizes cross-check against file size and each other | The original files | not started |
| Gameplay constants | Value recovered from script defaults / native code / config, cited | The original files | not started |
| Player movement | Tick-by-tick position/velocity error vs original trace | Original game on Windows (traces) | not started |
| Grapple | Attach point, swing path, release velocity vs original trace | Original game on Windows (traces) | not started |
| Levels | Geometry/transforms match original placement | Original maps (converted locally) | not started |
| Story flow | Kismet-driven event order matches original | Kismet graphs + playthrough | not started |

## Known deviations

None recorded yet. Every intentional or known deviation must be listed here with its reason.

## Trace format (planned)

One sample per simulation tick:
`t, input{move, look, jump, grapple, ...}, position, velocity, camera{rotation, fov}, grapple{state, anchor}, grounded`.
Original-game and runtime traces share the schema; the replay harness reports per-field error over time.
