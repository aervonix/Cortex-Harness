# Answered in the third tooling step, on 2026-11-14

This step is not a numbered step of the build plan: it built the third round of developer tooling that wave 6's sessions asked for, and fixed the nightly run of 2026-11-14. Its records are here, in a folder named for the job.

## The owner's answers

| Question | Answer |
|---|---|
| What is "a by-name break that another break in the list already covers", for the pre-flight? | A break made unable to fail by a second guard in the code, not by an overlapping break: step 13a closed the Windows credentials file by name, then its whole folder, and the break of the first could no longer fail. Only running the breaks shows it, so the pre-flight does not look for it. `mutate`'s NOT CAUGHT says, for a break whose list names its tests, that another guard may now cover it. |
| Removing the default bodies of the wrapped traits, with a `rules` check to keep them out, for item 2? | Agreed, with the `tool_call` fix. |
