# 0028: Enforced values fail closed

- **Status:** Accepted. Decided in the review of step 3, on 2026-11-12.
- **Date:** 2026-11-12

## Context

The protocol grows by addition ([0008](0008-rpc-protocol.md)). To make that safe, a receiver ignores a field it does not know and keeps an enumeration value it does not know. Step 3 built that rule into every type.

For most of the protocol that is right: a field that only informs can be skipped. For a value the harness **enforces**, it is a security hole. A lease is the list of what a session may do. If a newer build adds a restriction and an older build reads the lease, "ignore what you do not know" turns a narrower lease into a wider one, silently. A lease is read by more than one build in ordinary use: a session log opened by another version, a supervisor and its Employees, a client and a server.

## Options considered

| Option | Assessment |
|---|---|
| Tolerant everywhere, and leave it to `cortex-policy` to notice | The policy engine cannot notice a field the decoder already threw away. |
| Tolerant decoding that keeps unknown parts, with a check afterwards | Works only if every later reader remembers the check. One forgotten call site is the hole again. |
| **Strict types for enforced values: the decoder itself refuses** | There is no lenient way to read a lease, so nothing can forget to check. The cost is that enforced types cannot grow by plain addition. |

## Decision

**The types the harness enforces refuse what they do not understand, and are refused whole.**

- **Which types.** A lease, a grant (and so a sub-lease, which is a lease), a permission rule, a sandbox policy and its path sets, a budget, the hard limits, a stop condition, an expiry and a grantor.
- **Unknown fields are an error** in every one of them.
- **Unknown values are an error:** a level, an effect, a rule action, a network setting, a kind of stop condition, of expiry and of grantor.
- **The whole value is refused.** Nothing is ignored, nothing is kept, and nothing is applied in part. A lease with one unknown stop condition is not a lease with one fewer.
- **The refusal has its own code:** `lease_not_understood`, with the kind `permission`. The message names the field or the value.
- **Everything that only informs stays tolerant,** as built: events, items, usage, model info, notices, session state.

Two enumerations sit on both sides. A **level** and an **effect** appear in enforced values (a grant's level, a rule's effect) and in informational ones (a session's state, an approval prompt). As types they stay open, so that a client can show a session whose level it has never heard of. Inside a grant or a rule a value that is not listed is refused. The generated schema says the same: the field in the grant lists the known levels and nothing else.

**What this costs.** Adding a field or a value to an enforced type is a breaking change for any reader that might meet it: an older build refuses the lease. That is the intent, and it means such an addition is gated: by a capability that the sender checks before it sends the new form, or by a new protocol version.

**Related, and not changed by this record:** a request to *set* a level the server does not know is refused with `unknown_value`, and an answer to an approval request that the server does not know is a denial. Both were already "open is not accepted".

## Why

- A restriction that is dropped is worse than a request that is refused. The second is visible and safe.
- Making the decoder strict puts the rule in one place that every reader goes through.
- It costs the tolerant parts nothing: the two rules apply to different types.

## Where it is built

`crates/cortex-protocol/src/lease.rs`. The enforced structs forbid unknown fields; their own enumerations and the stop-condition union have no catch-all variant; `Lease::from_wire`, `Grant::from_wire` and `Rule::from_wire` return the refusal as an error object. Tests: `a_lease_with_an_unknown_restriction_field_is_refused`, `a_lease_with_an_unknown_restriction_value_is_refused`, `a_grant_with_an_unknown_scope_is_refused`, `a_rule_in_a_message_is_enforced_too`, and, for the schema, `the_schema_refuses_what_the_types_refuse`.

Step 12 builds the policy engine on these types, and step 8 writes leases to the session log. Both read them through the same decoder.

## Revisit when

An enforced type needs to grow often enough that refusing older readers is a burden. The answer then is a way to mark an addition as safe to ignore *by the sender, in the value itself*, never a lenient reader.
