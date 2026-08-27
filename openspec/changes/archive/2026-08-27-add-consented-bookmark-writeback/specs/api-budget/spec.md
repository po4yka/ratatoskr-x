## ADDED Requirements

### Requirement: Provider budgets are isolated by operation class

Every durable budget window SHALL belong to one closed operation class. Bookmark mutation and all
provider requests needed to resolve its uncertain outcome SHALL use a hard `bookmark_write` class;
all existing synchronization and compliance requests SHALL remain in read classes. Usage, cap,
window rollover, refund, and concurrency accounting SHALL be evaluated by account and class so no
class can consume or borrow another class's allowance.

#### Scenario: Write usage does not reduce read allowance

- **WHEN** a bookmark-write reservation and a read reservation occur for one account in the same wall-clock window
- **THEN** each class reports only its own usage and retains its independently configured remaining allowance

#### Scenario: Exhausted read class does not block available write class

- **WHEN** an account has exhausted its read allowance but its bookmark-write class can admit the requested cost
- **THEN** the write reservation is accepted against only the bookmark-write row

#### Scenario: Exhausted write class cannot borrow read allowance

- **WHEN** the bookmark-write class cannot admit a cost while a read class has remaining allowance
- **THEN** the write reservation is refused with its own reset instant and no read usage changes

#### Scenario: Racing reservations serialize within class

- **WHEN** concurrent read and bookmark-write reservations race for one account
- **THEN** each class independently remains within its cap and reservations in one class do not lockstep or alter the other class's usage

### Requirement: Budget eligibility can be inspected without reservation

The budget gate SHALL expose a non-consuming eligibility result for dry-run admission using the same
account, class, clock, window, cap, and requested-cost calculation as a live reservation. Inspection
SHALL return whether the cost would currently fit and the applicable reset instant, but SHALL NOT
create or modify a window row and SHALL NOT promise that a later live reservation will still succeed.

#### Scenario: Eligible inspection leaves usage unchanged

- **WHEN** a dry run inspects bookmark-write cost that currently fits its class
- **THEN** the result reports eligible with the evaluated instant and reset while persisted usage and rows remain unchanged

#### Scenario: Exhausted inspection matches live refusal class

- **WHEN** a dry run inspects bookmark-write cost that exceeds the class's remaining allowance
- **THEN** it reports exhausted with the same reset instant a live reservation would report at that moment and persists no charge
