| Item | Who | When |
|---|---|---|
| **A call is decided before earlier calls of the same response have run.** Two shell calls in one response, `ln -s . d` and then `cp evil d/.bashrc`: the second is decided while `d` does not exist, and runs after the link is made, past every check of what a path leads to. Decide a call that runs a command when it is about to start, or again after a call with side effects has run. Found by the final independent review of the fixes to step 12. | Step 13a | In step 13a |
