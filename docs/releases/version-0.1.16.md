# Pyre - 0.1.16

`0.1.16` prevents local migration and introspection commands from triggering a libSQL connection teardown bug that can crash Linux musl builds with `SIGSEGV`. Short-lived CLI connections now remain alive until process exit while awaiting an upstream libSQL fix.
