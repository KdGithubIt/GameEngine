# Typed UI authoring contract

## UI Contract Designer GUI

Open a project in Engine Editor, select **Authoring Tools**, then open **UI
Contract Designer**. The designer appears as a modeless window inside the current
editor process; no standalone designer executable or Cargo command is required.

Load the target `.ui.json` document first. The designer reads its stable node IDs
and provides selection controls instead of requiring users to type focus targets.

The GUI includes:

- typed Text, Number, and Flag binding candidate tables
- named project UI event candidates
- initial-focus selection from the loaded UI document
- explicit Up, Down, Left, and Right focus links
- a per-node focus preview
- inline validation and save blocking for duplicate names, missing nodes, and
  conflicting directional links
- New, Open, Save, and Save As for `.ui-contract.json`

`engine_authoring::UiAuthoringContract` is the persisted format used by the GUI.
Candidate lists remain deterministic and the same validation API is available to
tests, CLI tools, runtime integration, and future automation clients.
