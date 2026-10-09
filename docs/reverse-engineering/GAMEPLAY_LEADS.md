# Gameplay evidence database

Rules: never invent constants. Each value cites its source. Confidence: CONFIRMED / STRONG / TENTATIVE / UNKNOWN.

| Subsystem | Evidence source | Observed names | Known behaviour | Constants found | Confidence | Rust implication | Open questions |
|---|---|---|---|---|---|---|---|
| Player movement | Symbols (native) | `APawn::physWalking`, `APawn::physFalling`, `APawn::CalcVelocity`, `APawn::NewFallVelocity`, `APawn::stepUp`, `AUDKPawn::CalcVelocity`, `AUDKPawn::physFalling`, `AUDKPawn::performPhysics`, `AUDKPawn::GetGravityZ` | ASAMU registers no movement natives; UDKBase overrides CalcVelocity/physFalling. Expected: stock UE3/UDK native pawn physics driven by script parameters | none | TENTATIVE | Reimplement the native algorithms (from Ghidra reading, in our own code) and feed them script defaults | Which pawn class does ASAMU use, and what are its defaults? Which physics mode while grappling? |
| Ground acceleration | — | — | — | none | UNKNOWN | | |
| Air control | — | — | — | none | UNKNOWN | | |
| Jump | — | — | — | none | UNKNOWN | | |
| Gravity | — | — | — | none | UNKNOWN | | WorldInfo/Zone gravity override? |
| Collision dimensions | — | — | — | none | UNKNOWN | | Cylinder radius/height |
| Grapple target acquisition | Symbols; localization asset names | no grapple native symbols; VO asset `Narrator_Sanctuary_GrappleSymbol`, `Maddie_StarHaven_WithoutTheGrapple` | Grapple exists as a mechanic; implementation not native | none | STRONG (script-side) | `asamu-player::grapple` | Locate grapple classes in the ASAMU script package |
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
| Suit abilities | Localization asset names | `rocketBoots.Narrator_StarHaven_RocketBoots_*`, `Maddie_FindingRocketBoots_*`, `Narrator_IceCave_RocketBootsBreak`, `Narrator_Workshop_AdventureSuit_*` | Rocket boots are acquired in Star Haven and break in the Ice Cave (from asset names only) | none | TENTATIVE | Ability unlock state in `asamu-game` | Confirm from script classes / Kismet |
| Save / progression | Symbols | `UASAMUSystemSettingsManager` (settings only) | native settings get/set/save | none | CONFIRMED (names) | | Save game format? |
