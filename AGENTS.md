# AGENTS.md

## Purpose
Complete the current task with the minimal sufficient solution.
Prohibit over-engineering.
Planning can be aggressive, but execution must be lightweight.
Designs that cannot prove necessity are not done by default.
Tests that cannot prove necessity are not added by default.
Planning can be ambitious, execution should be lightweight, constraints come first.  

## Workflow
1. Understand the requirements first, then take action. Do not modify code first and guess the intent afterward.
2. Higher reasoning can be used in the planning phase. In the execution phase, default to medium-low reasoning, or switch to a lighter model for implementation.
3. Do not keep the highest reasoning mode on throughout.
4. Do not default to spinning up multiple Agents in parallel. Complete one task in a single thread first, then decide whether to split it.
5. Only enable skills that are necessary to complete the task. Do not install heavy-process skills.
6. Produce a minimal plan first, then execute. The plan must clearly state:
   - Goals
   - Non-goals
   - Acceptance criteria
   - Scope not to be changed
7. Code push: pass Testing > Pre-push gate.

## Agent skills

### Issue tracker

Issues and specs are tracked as local Markdown files under `.scratch/`. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the default canonical triage labels: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

This is a single-context repository: read root `CONTEXT.md` and relevant ADRs under `docs/adr/` when they exist. See `docs/agents/domain.md`.

## tasks/bugs/features/tickets rules

### creation

Whenever a new task or bug or feature is being created it *MUST* contain acceptance criterias. Write them like this:

```
## acceptance criteria:
```

### finishing

If the ticket contains acceptance criteria (marked with AC1, AC2, and so on) you *MUST* fulfill those, otherwise your work is NOT done.

## Acceptance Criteria Formats

### Given-When-Then (Gherkin)
Structure:

```
Given [precondition/context]
When [action/event]
Then [expected outcome]
And [additional outcome]
```

Example - Login:

```
Scenario: Successful login with valid credentials
Given I am on the login page
And I am a registered user with email "user@example.com"
When I enter "user@example.com" as email
And I enter "ValidPassword123" as password
And I click the "Sign In" button
Then I should be redirected to the dashboard
And I should see a welcome message "Welcome, John"
And my last login time should be updated
```
```
Scenario: Failed login with invalid password
Given I am on the login page
And I am a registered user
When I enter valid email
And I enter incorrect password
And I click the "Sign In" button
Then I should see error message "Invalid email or password"
And I should remain on the login page
And account lockout counter should increment
```
```
Scenario: Account lockout after 5 failed attempts
Given I am on the login page
And I have failed login 4 times
When I fail login a 5th time
Then my account should be locked for 30 minutes
And I should see message "Account locked. Try again in 30 minutes"
And I should receive a security alert email
```
### Checklist Format

Example - Checkout:
```
## User Story: Guest Checkout

### Acceptance Criteria
- [ ] Guest can proceed to checkout without creating account
- [ ] Email field is required and validated
- [ ] Shipping address form includes: name, address, city, state, zip, country, phone
- [ ] All required fields show validation errors if empty on submit
- [ ] Address validation API is called and suggests corrections
- [ ] Shipping options are displayed with real-time prices
- [ ] Order summary shows all items, quantities, and prices
- [ ] Payment form accepts credit card information
- [ ] Order is created upon successful payment
- [ ] Confirmation page shows order number and details
- [ ] Confirmation email is sent within 1 minute
- [ ] Guest is offered option to create account after order
```

### Scenario-Based

Example - Shopping Cart:

```
## User Story: Add to Cart

### Happy Path
- User selects product variant (size, color)
- User clicks "Add to Cart"
- Product is added to cart with correct quantity
- Cart icon updates with item count
- Mini cart shows added product confirmation
- User can continue shopping or go to cart

### Edge Cases
- Out of stock: "Add to Cart" button disabled, shows "Out of Stock"
- Limited stock: Shows "Only 3 left" warning
- Maximum quantity: "Only 5 per customer" message
- Already in cart: Updates quantity instead of adding duplicate
- Variant not selected: Prompts to select variant first

### Error Scenarios
- Network error: Shows "Unable to add. Please try again"
- Inventory changed: Shows "Sorry, this item is no longer available"
- Session expired: Redirects to login, preserves cart on return
```

### Best Practices

Writing Good Acceptance Criteria

✅ Do:

- Be specific and measurable
- Cover happy path AND edge cases
- Include error scenarios
- Make each criterion independently testable
- Use consistent language
- Consider user perspective
- Include data validation rules
- Specify timing requirements (if applicable)

❌ Don't:

- Be vague ("system should be fast")
- Skip error handling
- Assume implied knowledge
- Make criteria too granular (test case level)
- Forget about edge cases
- Mix multiple features in one criterion

Testability Checklist
- [ ] Can QA write a test case from this?
- [ ] Is the expected outcome clear?
- [ ] Can we objectively say pass/fail?
- [ ] Are values specified (not "valid input")?
- [ ] Are timing requirements specified?

Completeness Checklist
- [ ] Happy path covered?
- [ ] Error cases covered?
- [ ] Edge cases covered?
- [ ] Empty/null states covered?
- [ ] Permission/access scenarios covered?
- [ ] Mobile/responsive scenarios covered?

## Test-Driven Bug Fixes (Zero Regressions)

Every bug fix **must** include an automated regression test. Under no circumstances should a bug be fixed without a corresponding test demonstrating the issue and preventing recurrence.

### Mandatory Bug-Fix Protocol

Whenever you are tasked with fixing a bug or unexpected behavior, execute the following steps in strict order:

1. **Reproduce First (Failing Test):**
   - Write a minimal automated test (unit, integration, or end-to-end depending on scope) that explicitly reproduces the reported bug.
   - Run the test suite and confirm that this new test **fails** due to the reported issue.
   - Do **not** modify application code before completing this step.

2. **Implement the Fix:**
   - Apply the minimal, cleanest change necessary to resolve the root cause.
   - Avoid unrelated refactoring or collateral changes in the same diff.

3. **Verify Green:**
   - Run the new test and verify it passes.
   - Run the entire existing test suite to ensure no regressions were introduced.

4. **Document the Test:**
   - Add a brief comment or docstring to the new test referencing the bug description or issue ID (e.g., `// Regression test for issue #123: prevents null dereference on empty input`).

## Testing

### Pre-push gate

Code changes only; docs-only changes exempt. Before push:
1. Run focused proof.
2. Pass `.github/workflows/ci.yml` required checks: `cargo test --workspace --locked`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo fmt --all --check`.
3. Run affected mode checks (e.g. coverage, `--no-default-features`).
Missing/failing check: do not push; report gap.

Tests only serve to verify the current changes.
Tests are not responsible for filling historical coverage gaps or designing future test systems.

1. Prioritize running existing tests related to this change.
2. If existing tests can prove the change is correct, do not add new tests.
3. Only add new tests in the following two cases:
   - This change modified behavior, but existing tests don't cover it
   - User explicitly requires adding tests
4. New tests cover at most 1 main path of the actual change this time, and if necessary, add 1 key failure path.
5. Prohibit expanding test scope for completeness.
6. Prohibit using the opportunity to fill tests for unrelated modules.
7. Prohibit introducing new test frameworks, tools, or infrastructure.
8. Prohibit writing large snapshots, parameterized matrices, or end-to-end suites.
9. Prohibit writing tests for boundaries not required by the current needs.
10. Prohibit modifying tests first and then forcing product behavior to become more complex.
11. Prohibit using green tests as a reason to continue adding abstraction.

Before adding any test, must be able to answer:
- Which accepted requirement is this test verifying
- If removed, can existing tests no longer detect this regression
- Is it more complex than the implementation itself

If test code is longer or more convoluted than the implementation code, default to considering it over-engineering; delete the test or shrink the implementation.
