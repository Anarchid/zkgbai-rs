# Fight licences

`fightlicense` is an experimental operator feature, **off by default** on main
and wip. Enable it through the operator Features panel or the AI option
`enable=fightlicense`; disable it with the panel or `disable=fightlicense`.

Ordinary ground Assault/Hover squads and assault-doctrine taskforces (including
shieldballs) need a positive local combat assessment before deliberately entering
an enemy firing envelope. Raiders, aircraft and specialist doctrines retain their
existing controllers. Reinforcements receive no permission from a distant squad's
commitment. They use checked transit and wait when no safe route is found.

At least 80% of assigned metal must gather within 400 elmos. Only gathered members
count in the forecast. Over 30 seconds, predicted enemy deaths must return at
least 30 metal and two thirds of predicted own harm. Own harm includes damage;
enemy chip damage alone earns no credit. Unsupported or stale assessments refuse
permission. Own shields get no absorption credit; enemy shield matchups are
unsupported. This is deliberately conservative and can refuse winnable fights.

A refused squad retains a stand-off position outside enemy reach. An advancing
enemy pushes it back; a retreating enemy never pulls it forward. Previously seen
firing hazards remain after losing sight until death or visible clearance.
A licence binds exact members, opposition and its frame deadline. New opposition
invalidates it; ordinary individual health/shield retreat does not cancel it early.

Changing the operator flag cancels grants and refusal caches, stops covered unit
queues and forces reassignment. Disabling restores taskforce follow orders.
Remembered hazards remain as history and are ignored while the flag is off.

Licence rosters, anchors, deadlines and hazards use `#[checkpoint::state]` and
strict non-default round trips. The small codec/derive crates are shared with the
checkpoint-enabled working tree. These release branches still advertise
`loadSupported=no`; this feature does not enable whole-bot save/load on them.
The checkpoint-enabled working tree persists the new squad and military fields
through its existing owner snapshots and rejects incompatible schemas.

The initial isolated fixtures preserved Knights facing Scalpels/Grizzly and
Aspises rallying across Grizzly fire. Twelve Knights also refused a Grizzly that
the baseline killed. No licence was granted in those fixtures; they establish
refusal/transit behavior, not successful live execution of an accepted commitment.
