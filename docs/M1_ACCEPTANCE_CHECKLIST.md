# M1 Acceptance Checklist

This is the release checklist that resolves the manual portions previously
split across Phase 17-D, Phase 19-C/D, and Phase 20-B. Automated coverage is
named where available; visual and device-dependent checks remain explicit so a
release operator can record the tested build and machine without overstating
CI coverage.

Acceptance record for the current Busters Lite proving build:

- Date: 2026-07-18
- Commit: uncommitted working tree (record the final commit before release)
- OS / GPU: Windows; GPU-dependent visual checks remain open below
- Operator: Codex automated acceptance run

## Editing Loop (17-D / 20-B)

- [ ] Open `examples/busters_lite` as a project; assets and arena scene appear.
- [ ] Open `assets/scenes/arena.scene.json`; hierarchy shows all entities.
- [ ] Add, rename, and delete a temporary entity; undo restores each change.
- [ ] Edit Transform, Camera, Light, Mesh, and Material values.
- [ ] Play, stop, save, restart the editor, and reopen with identical authoring data.
- [ ] Resize Game View during Play; rendering and camera aspect remain valid.
- [ ] Register an OBJ once; a second registration reports `asset.already_registered`.

Automated support: editor document/session tests, scene bridge tests, asset
registration tests, and sample project smoke tests run in the workspace suite.

## Runtime Wiring (19-C / 19-D)

- [ ] A Behavior Tree scene starts through editor Play without blocking diagnostics.
- [ ] Debug-line toggle changes runtime debug rendering while playing.
- [ ] Game View focus routes input; focus loss releases held keys.
- [ ] Time advances while playing and stops with Play mode.
- [x] `busters_lite` completes title -> sortie -> combat -> result.
- [ ] Player movement, combo attack, lock-on cycle, ally AI, and enemy group work.
- [ ] Pause freezes gameplay and resume continues it.
- [x] Mission clear creates/updates save slot 0 and the next run shows the clear count.

Automated support: runtime input/focus tests, time tests, Behavior Tree tests,
and `busters_lite` authoring/state tests run in the workspace suite.

## Packaging and Performance (Phase 63)

- [ ] Build `player` in release mode and click editor `Package`.
- [x] The output contains the game executable, project documents, manifest, and assets.
- [x] Launch the packaged game and complete one `busters_lite` mission.
- [x] Missing start scene and missing manifest asset block packaging with diagnostics.
- [ ] Observe gameplay update last/max timing in the HUD; investigate sustained values
      above 1,000 microseconds on the release test machine.

Automated support: package planning/copy-layout tests and the four workspace
quality gates.

### 2026-07-18 evidence

- Built `target/release/player.exe` and the release Busters GameModule.
- Packaged 24 project files through the same
  `package_project_with_game_module` operation used by the editor, producing
  `target/release/busters_lite_package/build_report.json` with `success: true`.
- Started the packaged `game.exe` from its distribution directory and observed
  it remain running for eight seconds; the acceptance harness then stopped the
  exact process it launched.
- Ran `runtime::tests::busters_mission_completes_and_persists_clear_count`
  against the packaged project directory and packaged `game_module.dll`. The
  test crossed all four scenes, defeated the authored enemy group, wrote slot
  0 with `clear_count = 1`, and verified that a second runtime generation
  loaded that value.
- The checked runtime/package items above are therefore backed by executable
  package evidence. Unchecked editing, input-device, visual, and timing items
  still require a human release-operator pass and are intentionally not
  represented as complete.
