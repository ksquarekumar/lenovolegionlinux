# Reference: message kinds

| Kind | Who sends it | The receiver gets it | The receiver does |
| --- | --- | --- | --- |
| `note` | any peer | in the next prompt | reads it; acks it |
| `hint` | any peer | in the next prompt | thinks about it; acks it |
| `reminder` | a member | in the next prompt, with the count of misses | does the step; acks it |
| `notice` | a member | in the next prompt | reads the record of a step that the member did |
| `result` | a member | in the next prompt | reads each open item |
| `soft-block` | a thread-observer member | at one reply end | does the step, or acks with a reason |
| `hard-block` | a thread-observer member | at each reply end and each call of its tool | fixes the fault, then acks it |

Archetypes and the kinds they may send:

| Archetype | Kinds |
| --- | --- |
| `main-agent` | `note` |
| `thread-observer` | all kinds |
| `ui-composer` | `note`, `result` |

A team flow (`flow.may`) can only remove kinds from this list.
