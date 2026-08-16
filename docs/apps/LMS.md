# lms — a learning platform (courses, auto-graded quizzes, gradebook, certificates)

A multi-role learning-management app — the "fairly complex" showcase that ties
several capabilities together over one consistent source of truth. An
**instructor** builds courses out of **lessons** (Markdown) and **quizzes**
(multiple-choice); a **student** enrolls, reads lessons, and takes quizzes that
are **auto-graded** by the **`quiz:grade`** component. From those graded
submissions the app derives everything that must reconcile: a student's
**progress**, the instructor's **gradebook** (per-student scores + a class
distribution chart via **`svg:chart`**), and a completion **certificate** (a PDF
via **`pdf:codec`**) issued once a student passes every quiz.

Same shape as the other showcases, at a larger scale: one **`lms-domain`** HTTP
component that exports `wasi:http` and imports only WIT contracts — the composed
**auth-guard** (`auth:identity`) for accounts + roles, **`records:store`** for
five collections, **`quiz:grade`** for grading + gradebook stats, **`pdf:codec`**
for certificates, and **`svg:chart`** for the gradebook chart. No bespoke auth,
storage, grading, PDF, or charting. The frontend is a **React + shadcn/ui** SPA
with two role modes.

![The lms on two roles: a student opens a course, reads lessons, and answers a multiple-choice quiz — a green “100%” badge and a full progress bar appear, and a Certificate button unlocks; the instructor opens the same course and sees a gradebook table (each student's per-quiz scores + average, a green check for passing all) and a server-rendered bar chart of the class average per quiz. A live recording of the running React app.](../media/lms.gif)

## The capability model

**Two roles** (self-assigned at register in the demo): `instructor` and
`student`. Courses are visible to everyone; an instructor only **edits their own**
courses and only sees **their own** gradebook, and the quiz **answer key is
stripped** from a student's view of a course. Every write checks the caller's
token (`authorizer::introspect`); grading and the certificate are gated on
enrollment / passing.

## The data model

- **courses** — `{code, title, description, instructor}`.
- **lessons** — `{course, title, body, order}` (Markdown content).
- **quizzes** — `{course, title, pass_mark, questions:[{prompt, options[], answer}]}`.
  The `answer` index is instructor-only.
- **enrollments** — `{course, student}`.
- **submissions** — `{quiz, course, student, answers[], correct, total, score_pct,
  passed}`. Written on every attempt; **the best per quiz** is what counts.

A fresh instructor account is seeded a demo course ("Intro to WIT Components")
with lessons and a quiz, so a student can enroll and take it immediately.

## Grading, and why it reconciles

The scoring math is the **`quiz:grade`** component — `grade(answers, key,
pass_mark)` returns `{correct, total, score_pct, passed}`, and
`distribution(scores, pass_mark)` rolls a cohort's percentages into gradebook
stats (mean, median, spread, pass count, a 5-bin histogram). Because *every*
number is derived from the same submissions through this one component:

- a student's **progress** (best score per quiz, completion, eligibility),
- the instructor's **gradebook** (per-student average, per-quiz mean, the chart),
- and the **certificate** threshold (passed every quiz)

all agree by construction. The e2e pins exactly this: a student's `100%` shows up
identically in their progress and the instructor's gradebook, the certificate
issues only after passing all quizzes, and a not-yet-passing student is refused.

## Run it

```bash
just host-lms     # composes the component, builds the React UI, serves on :3048
# register as `instructor` (seeded a demo course) or `student` (enroll + take it).
just e2e-lms      # multi-role flow + grade reconciliation + certificate gating
```

The frontend lives in `examples/lms/ui` (Vite + React + shadcn/ui); it renders an
instructor gradebook (table + server-rendered chart) and a student quiz-taker.

## Rungs left

- **Weighted grades** — per-quiz weights and a letter-grade scale.
- **More question types** — multi-select, short-answer (needs manual grading).
- **Cohorts + due dates** — sections, deadlines, and late penalties on a
  `sched:timer`.
- **Data export** — a course's gradebook as a `.zip` of CSVs via `zip:archive`.
