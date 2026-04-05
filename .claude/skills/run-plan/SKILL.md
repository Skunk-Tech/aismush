---
name: run-plan
description: Execute a plan autonomously with parallel agents. Triggers on "run plan", "execute plan", "execute the plan", "start the plan".
---

<run-plan>

You are the Plan Orchestrator. Execute the user's plan using specialized agents with DAG-based parallel execution.

## 1. Find the plan

Search for plan files in order:
1. If a plan file path was mentioned in this conversation, use that
2. Glob `.claude/plans/*.md` (project directory)
3. Glob `~/.claude/plans/*.md` (user home directory)

Read the most recently modified plan file. If none found: "No plan found. Create one first — ask me to plan something and I'll use EnterPlanMode."

## 2. Parse steps

From the plan file extract:
- **Title**: first `#` heading
- **Steps**: sections starting with `## Step`, `## 1.`, `## Phase 1:`, or similar numbered `##` headings
- **Verification**: `## Verification` or `## Testing` section if present

Skip metadata sections (Context, Background, Files to modify, etc.).

For each step, check if it explicitly depends on another step ("after step 1", "depends on step 2", "using output from step 3"). Also check if files modified by one step are read by another.

## 3. Build dependency graph

For each step, list which step IDs must complete before it can start.

**Default: steps are INDEPENDENT** unless content clearly indicates a dependency. This maximizes parallelism. Only add a dependency when a step truly cannot run without another step's output.

## 4. Map agents

Match each step to a specialized agent:

| Content keywords | subagent_type |
|---|---|
| Rust, Cargo, Tokio, Hyper, proxy | rust-expert |
| Database, SQLite, migration, schema | data-engineer |
| JavaScript, Workers, frontend JS | javascript-expert |
| Dashboard, HTML, CSS, UI | frontend-engineer |
| Backend, API, routing, handler | backend-engineer |
| Bug, debug, error, investigate | debugger |
| Test, testing, verify | test-runner |
| Review, audit, quality | code-reviewer |
| Everything else | general-purpose |

## 5. Create tasks and confirm

Use **TaskCreate** for each step with:
- subject: `Step N: [brief title]`  
- description: the full step content from the plan

Then use **TaskUpdate** with `addBlockedBy` to wire up dependencies.

Display the execution plan to the user:

```
PLAN: [title] — [N] steps

Step 1: [title] → [agent] (no deps — ready)
Step 2: [title] → [agent] (no deps — ready)
Step 3: [title] → [agent] (blocked by: 1)
Step 4: [title] → [agent] (blocked by: 2, 3)

[M] steps ready immediately, [K] blocked
```

Ask with **AskUserQuestion**: "Execute this plan?" — Options: "Go", "No"

If No: stop immediately.

## 6. Execute the DAG

Loop until all tasks are complete or failed:

1. Call **TaskList** to find tasks that are `pending` with empty `blockedBy`
2. For each ready task:
   - **TaskUpdate** status to `in_progress`
   - Launch **Agent** with the mapped `subagent_type`
   - Prompt: step description + plan context + what prior steps accomplished
   - If ONE ready task: launch foreground (wait for result)
   - If MULTIPLE ready tasks: launch ALL in a single message with `run_in_background: true`
3. When an agent completes:
   - **TaskUpdate** status to `completed`
   - Record what it accomplished (files changed, key outcomes)
   - Check TaskList — new tasks may now be unblocked
4. If an agent fails:
   - **TaskUpdate** status to `completed` with failure note
   - Ask user: Continue (skip dependents), Retry, or Abort?
   - If abort: stop everything

**Agent prompt template:**
```
You are executing Step [N] of a plan: "[plan title]"

YOUR TASK:
[full step content]

COMPLETED STEPS:
[for each done step: "Step X ([title]): [what was accomplished]"]

Execute this step completely. Fix any errors before finishing. Report what you did and what files you changed.
```

## 7. Verify and report

After all steps complete:
- If plan has a Verification section: execute those checks
- If Rust files were changed: run `cargo check && cargo test`
- If JS files were changed: check for syntax errors

Display final summary:
```
PLAN COMPLETE: [title]

Step 1: [title] ............ DONE
Step 2: [title] ............ DONE  
Step 3: [title] ............ DONE
Step 4: [title] ............ FAILED: [reason]

Verification: cargo test — 51 passed, 0 failed

[X/N steps completed]
```

## Rules

1. ALWAYS confirm before executing — never skip the Go/No prompt
2. Use the TaskCreate/TaskUpdate system for ALL state tracking
3. Default to INDEPENDENT steps — only add dependencies when clearly required
4. Pass completed step summaries to each agent so they have context
5. If a step fails and others depend on it, skip the dependents (don't try to run them)

</run-plan>
