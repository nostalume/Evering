# Scan

This document defines the scanning behavior that AI must follow
when generating engineering documentation.

## General Rules

- Scan **only** the files or directories explicitly provided in the prompt.
- Treat the given context as **authoritative**.
- Do not infer behavior from external knowledge or unstated intent.
- If information is insufficient, explicitly mark it as *uncertain*.

---

## Invariants

### Goal
Identify conditions that must hold for the system to remain correct or safe.

### Extraction Rules
- Extract all assumptions that, if violated, may result in:
  - Undefined Behavior (UB)
  - Panic or runtime failure
  - Silent logical corruption
- Classify invariants by severity where applicable.

### Grouping
- Group invariants into `docs/invariants/{name}.md`,
  where `{name}` is specified in the prompt.
- Each invariant should:
  - Be stated declaratively
  - Avoid implementation details
  - Indicate its scope (global / module / function-level)

---

## Design

### Goal
Describe how the system is structured to satisfy the invariants.

### Extraction Rules
- Summarize responsibilities of modules and components.
- Describe key data flow and control flow.
- Focus on *intentional structure*, not incidental code patterns.
- Do not invent behaviors or rationale not evident from the code.

### Grouping
- Group design descriptions into `docs/design/{name}.md`,
  where `{name}` is specified in the prompt.
- Clearly separate:
  - What the component does
  - What it does **not** do (if evident)

---

## Relationship Between Design and Invariants

- Invariants describe *what must always be true*.
- Design describes *how the system is organized to ensure those truths*.
- Do not restate invariants as design, or vice versa.