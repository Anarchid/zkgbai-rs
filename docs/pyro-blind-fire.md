# Pyro blind fire

Pyros use coordinate Settarget against identified, remembered enemy buildings
that are outside LOS but already within weapon range. This is deliberately
Pyro-only: its flame weapon ignores terrain collision. Movement stays with the
existing controller. During a precision mission, the fallback only runs in the
firing phase and only targets that mission's building.

Another enemy contact within weapon range plus 150 elmos suppresses blind fire.
Known statics remain relevant; unseen mobile memories expire after 20 seconds.
Unidentified radar contacts also suppress it. The range check includes height
difference and a 16-elmo margin. Visibility, retreat, target loss, or leaving
range ends the override. The selection is reassessed every half-second.

The active unit/target/coordinate map uses `#[checkpoint::state]` and lives under
`Ai.precision`. It retains responsibility for clearing engine targets and does
not cancel a replacement target from another controller. Failed cancellations
retain ownership for retry. Disabling `precision` clears overrides at the next
update when command authority permits; reenabling reassesses current contacts.
Existing movement plans and prediction caches are preserved.

The component has a non-default checkpoint round-trip test, including ownership
recognition and target selection after restoration. Full-bot snapshot wiring is
separate existing work on these release branches; this change does not claim
engine-level continuation or enable loading unsupported saves.
