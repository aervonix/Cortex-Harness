| Item | Who | When |
|---|---|---|
| **Corners of matching a shell pattern against a name.** `NamePattern` lower-cases a name with `to_lowercase`, so a letter that becomes two (`İ`) is no longer matched by one `?`. A shell option the user sets (`dotglob`, `GLOB_DOTS`, `globstar`, `nocaseglob`) changes what a pattern matches, and the analysis does not know it. Found by the final independent review of the fixes to step 12. | Step 32 (hardening pass); the sandbox of step 26 holds the rest | In step 32 |
