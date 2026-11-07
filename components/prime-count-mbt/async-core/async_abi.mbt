// #region subtask

///|
priv struct SubTask {
  handle : Int32
  state : SubTaskState
}

///|
priv enum SubTaskState {
  Starting = 0
  Started = 1
  Returned = 2
  Cancelled_before_started = 3
  Cancelled_before_returned = 4
}

///|
fn SubTaskState::from(int : Int32) -> SubTaskState {
  match int {
    0 => Starting
    1 => Started
    2 => Returned
    3 => Cancelled_before_started
    4 => Cancelled_before_returned
    _ => panic()
  }
}

///|
fn SubTask::from(code : Int32) -> SubTask {
  { handle: code >> 4, state: SubTaskState::from(code & 0xf), }
}

///|
/// The baseline intrinsic cooperatively waits in the component model and
/// returns only after the subtask reaches a terminal state.
fn SubTask::cancel(self : SubTask) -> SubTaskState {
  SubTaskState::from(subtask_cancel(self.handle))
}

// #endregion

// #region events

///|
priv enum EventCode {
  None = 0
  SubTask = 1
  StreamRead = 2
  StreamWrite = 3
  FutureRead = 4
  FutureWrite = 5
  TaskCancelled = 6
}

///|
fn EventCode::from(int : Int32) -> EventCode {
  match int {
    0 => None
    1 => SubTask
    2 => StreamRead
    3 => StreamWrite
    4 => FutureRead
    5 => FutureWrite
    6 => TaskCancelled
    _ => panic()
  }
}

///|
priv enum Events {
  None
  Subtask(Int32, SubTaskState)
  StreamRead(Int32, StreamResult)
  StreamWrite(Int32, StreamResult)
  FutureRead(Int32, FutureReadResult)
  FutureWrite(Int32, FutureWriteResult)
  TaskCancelled
}

///|
fn Events::new(code : EventCode, i : Int32, j : Int32) -> Events {
  match code {
    None => None
    SubTask => Subtask(i, SubTaskState::from(j))
    StreamRead => StreamRead(i, StreamResult::from(j))
    StreamWrite => StreamWrite(i, StreamResult::from(j))
    FutureRead => FutureRead(i, FutureReadResult::from(j))
    FutureWrite => FutureWrite(i, FutureWriteResult::from(j))
    TaskCancelled => TaskCancelled
  }
}

// #endregion

// #region waitable set

///|
#borrow(array)
extern "wasm" fn int_array2ptr(array : FixedArray[Int32]) -> Int32 =
  #|(func (param i32) (result i32) local.get 0)

///|
#derive(Eq, Hash)
struct WaitableSet(Int32)

///|
#deprecated
pub extend WaitableSet with Eq::{not_equal, equal}

///|
#deprecated
pub extend WaitableSet with Hash::{hash, hash_combine}

///|
fn WaitableSet::new() -> WaitableSet {
  WaitableSet(waitable_set_new())
}

///|
fn WaitableSet::drop(self : Self) -> Unit {
  waitable_set_drop(self.0)
}

///|
fn WaitableSet::poll(self : Self) -> (Events, Int32) {
  let payload : FixedArray[Int32] = FixedArray::make(2, 0)
  let event = waitable_set_poll(self.0, int_array2ptr(payload))
  (Events::new(EventCode::from(event), payload[0], payload[1]), payload[0])
}

// #endregion

// #region Future

///|
priv enum FutureReadResult {
  Completed = 0
  Cancelled = 2
}

///|
fn FutureReadResult::from(int : Int32) -> FutureReadResult {
  match int {
    0 => Completed
    2 => Cancelled
    _ => panic()
  }
}

///|
priv enum FutureWriteResult {
  Completed = 0
  Dropped = 1
  Cancelled = 2
}

///|
fn FutureWriteResult::from(int : Int32) -> FutureWriteResult {
  match int {
    0 => Completed
    1 => Dropped
    2 => Cancelled
    _ => panic()
  }
}

// #endregion

// #region Stream

///|
priv struct StreamResult {
  progress : Int32
  copy_result : CopyResult
}

///|
fn StreamResult::from(int : Int32) -> StreamResult {
  let progress = int >> 4
  let copy_result = CopyResult::from(int & 0xf)
  { progress, copy_result, }
}

///|
priv enum CopyResult {
  Completed = 0
  Dropped = 1
  Cancelled = 2
}

///|
fn CopyResult::from(int : Int32) -> CopyResult {
  match int {
    0 => Completed
    1 => Dropped
    2 => Cancelled
    _ => panic()
  }
}

// #endregion

// #region callback code

///|
/// Code to let the runtime know what to do in the callback
priv enum CallbackCode {
  Completed
  Yield
  Wait(WaitableSet)
}

///|
fn CallbackCode::encode(self : Self) -> Int32 {
  match self {
    Completed => 0
    Yield => 1
    Wait(id) => 2 | (id.0 << 4)
  }
}

// #endregion

// #region Component async primitives

///|
fn subtask_cancel(id : Int32) -> Int32 = "$root" "[subtask-cancel]"

///|
fn subtask_drop(id : Int32) = "$root" "[subtask-drop]"

///|
fn tls_set(tls : Int32) = "$root" "[context-set-0]"

///|
fn tls_get() -> Int32 = "$root" "[context-get-0]"

///|
fn task_cancel() = "[export]$root" "[task-cancel]"

///|
fn waitable_set_new() -> Int32 = "$root" "[waitable-set-new]"

///|
fn waitable_set_drop(set : Int32) = "$root" "[waitable-set-drop]"

///|
/// This poll is deliberately non-cancellable. Component-task cancellation is
/// delivered by the stackless callback instead of being consumed here.
fn waitable_set_poll(set : Int32, payload : Int32) -> Int32 = "$root" "[waitable-set-poll]"

///|
fn task_wakeup_stream_new() -> Int64 = "$root" "[stream-new-unit]"

///|
fn task_wakeup_stream_read(handle : Int32, ptr : Int32, len : Int32) -> Int32 = "$root" "[async-lower][stream-read-unit]"

///|
fn task_wakeup_stream_write(handle : Int32, ptr : Int32, len : Int32) -> Int32 = "$root" "[async-lower][stream-write-unit]"

///|
fn task_wakeup_stream_cancel_read(handle : Int32) -> Int32 = "$root" "[stream-cancel-read-unit]"

///|
fn task_wakeup_stream_drop_readable(handle : Int32) = "$root" "[stream-drop-readable-unit]"

///|
fn task_wakeup_stream_drop_writable(handle : Int32) = "$root" "[stream-drop-writable-unit]"

///|
fn waitable_join(waitable : Int32, set : Int32) = "$root" "[waitable-join]"

// #endregion
