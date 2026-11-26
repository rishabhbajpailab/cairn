# 0010: Scheduling with a single Durable Object alarm, no polling

- **Status:** accepted
- **Date:** 2026-11-23

## Context
Minute-polling loops keep the database awake (bad for Earth's active hours) and waste work.

## Decision
- One **Scheduler** DO sets its single alarm to `next_wake`: the earliest pending reminder (pushed past quiet hours) or enabled cron task.
- When the alarm fires it sends due reminders, enqueues cron firings for `(last_tick, now]` onto the TASKS queue, and re-arms.
- Writes to reminder or task items call `rearm`. An hourly Cron Trigger is the backstop.

## Consequences
- The database is touched only when something is due or has changed.
- Alarms are at-least-once. Reminder `state` and task run keys make duplicates harmless.
- A lost `rearm` call costs at most one hour of delay, until the backstop runs.
