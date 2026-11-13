| Item | Who | When |
|---|---|---|
| **`OLDPWD` in the shell's environment.** `cd -` before the line's own `cd`s, or after one that failed, goes to the directory `OLDPWD` names, which the analysis cannot know: `cd nope; cd -; cat .config/cortex-harness/credentials.toml` is decided from the wrong place. Take `OLDPWD` out of the environment a command runs with, as `CDPATH` is. Found by the final and last independent reviews of the fixes to step 12. | Step 13a | In step 13a |
