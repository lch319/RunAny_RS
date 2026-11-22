//! 输入框的撤销栈（单行 `text_input` 和多行 `text_area` 共用）。
//!
//! 做法就是「存快照」：内容 + 光标位置。输入框的内容最多几百个字符，
//! 存一份字符串比记「插入/删除了哪一段」简单得多，也不会因为分支走错而对不上。
//!
//! 两个细节：
//!
//! - **连着敲的几下合并成一步**：不合并的话，敲一句 20 个字要按 20 次 Ctrl+Z，
//!   用起来比没有还烦。判据是「距离上一次记录不到 700ms」。
//! - **程序换内容（切编辑对象、恢复默认）要清空**：不然撤销会把上一个菜单项的
//!   文字搬到这一个上，看起来像串了台。

use std::ops::Range;
use std::time::{Duration, Instant};

/// 一次可回退的状态。
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub text: String,
    /// 当时的选区（字节偏移）
    pub caret: Range<usize>,
}

/// 两次记录间隔小于这个值就合并（打字的手速上限）
const MERGE_WINDOW: Duration = Duration::from_millis(700);
/// 最多记多少步（够回退一屏半的内容，再多只是占内存）
const LIMIT: usize = 200;

#[derive(Default)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// 上一次记录的时刻（合并判断用）
    last: Option<Instant>,
}

impl History {
    /// 动手**之前**调：把「改之前的样子」记下来。
    pub fn record(&mut self, text: &str, caret: Range<usize>) {
        let now = Instant::now();
        let merge = self
            .last
            .map(|last| now.duration_since(last) < MERGE_WINDOW)
            .unwrap_or(false);
        self.last = Some(now);
        // 有新改动，重做链就作废了（和所有编辑器的行为一致）
        self.redo.clear();
        if merge {
            return;
        }
        self.undo.push(Snapshot {
            text: text.to_string(),
            caret,
        });
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
    }

    /// 撤销：把当前状态压进重做链，返回要恢复的状态。
    pub fn undo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let previous = self.undo.pop()?;
        // 撤销之后立刻再按撤销，不该被「合并窗口」吃掉
        self.last = None;
        self.redo.push(current);
        Some(previous)
    }

    /// 重做。
    pub fn redo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let next = self.redo.pop()?;
        self.last = None;
        self.undo.push(current);
        Some(next)
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.last = None;
    }

    /// 还有没有可撤销 / 可重做的（右键菜单要判断要不要点亮）。
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// 把记录下来的光标夹回内容范围内（内容被外部改短过时会发生）。
pub fn clamp_caret(caret: Range<usize>, len: usize) -> Range<usize> {
    let start = caret.start.min(len);
    let end = caret.end.min(len).max(start);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(text: &str, at: usize) -> Snapshot {
        Snapshot {
            text: text.to_string(),
            caret: at..at,
        }
    }

    #[test]
    fn undo_walks_back_and_redo_walks_forward() {
        let mut history = History::default();
        history.record("a", 1..1);
        // 相隔太久的两步不能被合并
        history.last = None;
        history.record("ab", 2..2);

        let back = history.undo(snap("abc", 3)).unwrap();
        assert_eq!(back.text, "ab");
        let back = history.undo(snap("ab", 2)).unwrap();
        assert_eq!(back.text, "a");
        assert!(history.undo(snap("a", 1)).is_none());

        let forward = history.redo(snap("a", 1)).unwrap();
        assert_eq!(forward.text, "ab");
    }

    #[test]
    fn fast_typing_collapses_into_one_step() {
        let mut history = History::default();
        history.record("", 0..0);
        // 同一瞬间连着记录：只有第一次生效
        history.record("f", 1..1);
        history.record("ft", 2..2);
        let back = history.undo(snap("ftw", 3)).unwrap();
        assert_eq!(back.text, "");
        assert!(history.undo(snap("ftw", 3)).is_none());
    }

    #[test]
    fn a_new_edit_drops_the_redo_chain() {
        let mut history = History::default();
        history.record("a", 1..1);
        let back = history.undo(snap("ab", 2)).unwrap();
        assert_eq!(back.text, "a");
        assert!(history.can_redo());
        history.last = None;
        history.record("a", 1..1);
        assert!(!history.can_redo());
    }

    #[test]
    fn carets_are_clamped_after_the_text_shrinks() {
        assert_eq!(clamp_caret(5..9, 3), 3..3);
        assert_eq!(clamp_caret(1..2, 10), 1..2);
    }
}
