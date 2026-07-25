# lms — a learning platform (LMS.md)

A multi-role learning-management app: an **instructor** builds courses of lessons
+ multiple-choice quizzes; a **student** enrolls and takes quizzes that are
**auto-graded** by the **`quiz:grade`** component. Grades roll up into a student's
progress, the instructor's **gradebook** (+ a `svg:chart` distribution), and a
completion **certificate** (`pdf:codec`). See [LMS.md](../../LMS.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA.

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui source
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/lms.rs             # e2e: multi-role flow + grade reconciliation + certificate gating
```

## Run

```bash
# from the repo root:
just host-lms            # composes the component + builds the UI + serves on :3048
```

Open `http://127.0.0.1:3048`: **register** as `instructor` (you get a seeded demo
course to manage + a gradebook) or as `student` (enroll in the course, take the
auto-graded quiz, watch your progress, and download a certificate once you pass).

```bash
just e2e-lms             # the multi-role flow + grade reconciliation
# work on the UI live:
cd examples/lms/ui && npm install && npm run dev
```
