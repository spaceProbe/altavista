"""M13.2 (``docs/open-questions.md`` question 107): a reference implementation of
``altavista.v1.LockstepService`` (``proto/altavista/v1/lockstep.proto``) -- **a test fixture,
not a deployed service.** See this package's own ``README.md`` for the full contrast with
``services/gmat-service`` (design-time only, but a real GMAT-hosting service) and
``crates/av-dynamics-service`` (the deployed Rust runtime): this package exists solely so
``crates/av-kernel``'s ``BINDING_KIND_CONTAINER`` executor path and
``tests/test_lockstep_ref.py`` have something real to Bind/Step/Reset/Shutdown against,
spawned as a local subprocess in tests -- never run standalone in a profile.
"""

from __future__ import annotations
