# Example: ui0 places a tests view

Trigger: the user types `/view tests` in the app's tab of thread 7.

1. ui0 reads the tests of the change set: `legionctl code tests list`.
2. ui0 makes one view: one line for each changed test with its last run and its kills; the failed tests first.
3. ui0 places the view: `legionctl views place 7 tests`; the app opens it as a tab of the thread.
4. ui0 sends `thread send main0 result "tests view placed: 2 changed tests, 1 with no kill"`.
