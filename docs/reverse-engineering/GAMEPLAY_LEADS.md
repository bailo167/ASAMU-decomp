# Gameplay evidence database

Rules: never invent constants. Each value cites its source. Confidence: CONFIRMED / STRONG / TENTATIVE / UNKNOWN.

| Subsystem | Evidence source | Observed names | Known behaviour | Constants found | Confidence | Rust implication | Open questions |
|---|---|---|---|---|---|---|---|
| Player movement | — | — | — | none | UNKNOWN | `asamu-player::movement` | Pawn class? physics mode? |
| Ground acceleration | — | — | — | none | UNKNOWN | | |
| Air control | — | — | — | none | UNKNOWN | | |
| Jump | — | — | — | none | UNKNOWN | | |
| Gravity | — | — | — | none | UNKNOWN | | WorldInfo/Zone gravity override? |
| Collision dimensions | — | — | — | none | UNKNOWN | | Cylinder radius/height |
| Grapple target acquisition | Symbols | none native | — | none | UNKNOWN | `asamu-player::grapple` | Expected in UnrealScript |
| Grapple distance | — | — | — | none | UNKNOWN | | |
| Grapple attachment rules | — | — | — | none | UNKNOWN | | |
| Grapple acceleration | — | — | — | none | UNKNOWN | | |
| Constraint / swing | — | — | — | none | UNKNOWN | | |
| Release momentum | — | — | — | none | UNKNOWN | | |
| Maximum speed | — | — | — | none | UNKNOWN | | |
| Camera | Config | `ASAMU.ASAMUViewportClient` | custom viewport client | none | CONFIRMED (name) | | |
| FOV | — | — | — | none | UNKNOWN | | |
| Checkpoints | — | — | — | none | UNKNOWN | | |
| Death / reset | — | — | — | none | UNKNOWN | | |
| Moving platforms | — | — | — | none | UNKNOWN | | |
| Triggers | — | — | — | none | UNKNOWN | | |
| Story sequencing | Config | maps list | — | none | TENTATIVE | | |
| Audio events | — | — | — | none | UNKNOWN | | |
| Save / progression | Symbols | `UASAMUSystemSettingsManager` (settings only) | native settings get/set/save | none | CONFIRMED (names) | | Save game format? |
