# Mission ownership and reactive defense

Conquest reserves its commander throughout the active operation, including
travel and intervals between construction jobs. Its home-energy partner is
reserved for home energy. Crew forecasts and economic job admission read the
same mission state. Forced conquest and team-selected conquest use this path.
No additional ownership ledger is maintained.

Travel cannot promise construction capacity. After arrival, mission work,
operator priorities, predicted build-guard work, factory-defense emergencies
and hard energy-stall work remain eligible. Ordinary economy projects wait.
Completion, cancellation, or disabling conquest releases the reservation;
a historical station alone does not reserve a worker. Compush keeps its
ordinary commander expansion and only reserves its designated energy partner.
Rush and tactical-controller reservations continue to apply independently.

A raided mex's admitted light turret now opens a `mex_defense` logistics issue.
The shared allocator supplies a crew and the dispatcher can interrupt ordinary
work to execute it. Existing spacing and defense budgets still govern admission.
The request is refreshed from raid memory, preserving tactical/operator deadlines.
An energy stall pauses staffing, and a started frame keeps its commitment even
if its mex is lost. An unstarted project loses this request when its mex memory
is removed. Disabling porc releases these automatic issues.

The economy operator view exposes `mex_defense.raided_mexes` and `projects`;
normal issue telemetry reports assigned crews and blockers. Regression tests
cover mission stages, role release, operator/stall exceptions, allocation during
travel, and reactive defense taking a worker from ordinary construction.
