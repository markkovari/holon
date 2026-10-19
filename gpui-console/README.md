# gpui-console

A desktop console for autonomous agents ([ADR-0119](../docs/adr/0119-an-agent-can-start-a-coding-session.md)):
it boots a dev lattice, embeds `agent-runtime`, and shows what the agents are doing on the *agent board*.
The list down the left is the sessions, a goal and everything it started; the tree beside it is that
session's agents and the coding sessions they opened, each with its model, tools, cost and time, and the
ones waiting for a person in the one colour that asks.

![The agent board over its scripted story: a lead on Opus hands the work to a planner and a writer, the planner opens two coding sessions on Sonnet, one stops to ask to write tests/limiter.rs, the list of what needs a person opens with Approve, This task, Whole session and Deny, and once it is approved both sessions, the planner and the reviewer finish while the costs count up. The list of sessions on the left keeps three goals from earlier today.](../docs/media/agents-board.gif)

![Giving a goal: New goal opens a form, the goal is typed and given to the planner, and the board follows the session it starts to the top of the list, through its coding session to Done.](../docs/media/agents-board-goal.gif)

![The same board in the light theme, with the approval waiting.](../docs/media/agents-board-light.png)

## Running it

GPUI wants macOS with the Xcode command line tools (the shaders are compiled at run time).

    cargo run --bin agents-board                       # the board over a scripted story: no lattice, model or key
    cargo run --bin agents-board -- --mode light
    HOLON_CONSOLE_BOARD=1 cargo run --bin gpui-console # the console, opened on the board
    cargo test --lib

The console starts a `comp-agentd` it finds built under `reconciler/target` (`cd reconciler && cargo build
-p comp-agentd`), on the mock provider unless told otherwise.

## Photographing it

The board photographs itself, from inside, because a process may read back its own windows without the
Screen Recording permission another process would need.

    agents-board --shoot DIR --at 4,11,24                    # PNGs at those seconds
    agents-board --form --shoot DIR --at 1.5                 # the New goal form
    agents-board --goal "Make the gateway answer 429" ...    # typed into the form two seconds in, and started
    agents-board --open --film 0.4:280:100 --gif FILE.gif    # a film: 280 frames 100ms apart, as one GIF
    BOARD_SLOWMO=8 agents-board                              # every motion eight times slower

The films above are `--open --film 0.4:280:100` and `--goal "Make the gateway answer 429 with a Retry-After
header" --film 0.4:150:100`, from a `--release` build (a debug one is too slow to keep up with the clock).
