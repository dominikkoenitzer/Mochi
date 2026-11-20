# Status

All six milestones in [PLAN.md](../PLAN.md) are done. The test suite drives
throwaway windows and injects real key presses on a real desktop, and CI runs
it: the end to end job fails if those tests skip, because a test that returns
early still reports as passed.

Cross monitor moves and an unplugged screen have only run against a simulated
second monitor so far, and still need real hardware. Supervised use on a real
desktop has already found one defect no test could have, a hidden window that
could be forgotten without being shown again; it is fixed and covered by a test.
