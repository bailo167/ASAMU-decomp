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
| Time trial | Localization + input config | `ASAMUHUDMovieTimeTrial`, `GFxASAMUPauseMenuTimeTrial`, `TimeTrialRestart` exec | A time-trial mode exists with its own HUD/pause menu | none | CONFIRMED (names) | Later milestone | Which maps; timing rules |
| Story sequencing | Config | maps list | — | none | TENTATIVE | | |
| Audio events | — | — | — | none | UNKNOWN | | |
| Suit abilities | Localization asset names | `rocketBoots.Narrator_StarHaven_RocketBoots_*`, `Maddie_FindingRocketBoots_*`, `Narrator_IceCave_RocketBootsBreak`, `Narrator_Workshop_AdventureSuit_*` | Rocket boots are acquired in Star Haven and break in the Ice Cave (from asset names only) | none | TENTATIVE | Ability unlock state in `asamu-game` | Confirm from script classes / Kismet |
| Input bindings | `Config/DefaultInput.ini` `[Engine.PlayerInput]` | `GBA_Fire`→`StartFire \| OnRelease StopFire` (LMB); `GBA_PowerJump`→`PowerJumpKeyDown \| OnRelease PowerJumpKeyUp` (RMB); SpaceBar→`Jump \| OnRelease ReleaseJump` + `RocketBoostKeyDown`; `GBA_Sprint`→`StartSprinting \| OnRelease StopSprinting` (LShift); `use` (E/Enter); `Duck`/`UnDuck`; `QuickLoad` (F7); `TimeTrialRestart` (F8); gamepad `SmartJump` | Exec function names the ASAMU controller/pawn must implement: fire (grapple, TENTATIVE), charged power jump, releasable jump, rocket boost, sprint, quick-load, time-trial restart | `MoveForwardSpeed=1200`, `MoveStrafeSpeed=1200`, `MouseSensitivity=30.0`, `LookRightScale=300`, `LookUpScale=-250`, `DoubleClickTime=0.25`, `bEnableMouseSmoothing=true` | CONFIRMED (config values) | Map our input layer to these actions; reproduce PlayerInput axis scaling once the native/script path is read | Which class implements PowerJumpKeyDown/RocketBoostKeyDown? Is StartFire the grapple? |
| Save / progression | Symbols | `UASAMUSystemSettingsManager` (settings only) | native settings get/set/save | none | CONFIRMED (names) | | Save game format? |
