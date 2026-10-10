//! 毎フレーム起きる失敗を、原因ごとに初回だけログへ出すための印（ルール au2-workflow「ログと print」）。
//!
//! 原因は文字列のキーで区別する。キーにはフレーム番号のように毎回変わる値を混ぜない（混ぜると毎回「初回」になる）。
//! 「キャッシュを破棄」で全部忘れ、同じ原因をもう一度出す（lib.rs の `on_clear_cache`）。

use std::collections::BTreeSet;

use parking_lot::Mutex;

/// 覚えておく原因の数の上限。超えたら、その旨を 1 回だけ出して以降は何も出さない
pub const MAX_CAUSES: usize = 256;

/// ログの末尾に付ける断り書き
pub const SUFFIX: &str = "（同じ原因は以降出しません。「キャッシュを破棄」でまた出します）";

/// 上限に達したことを覚えておくための、ふつうのキーと重ならない印
const OVERFLOW: &str = "\u{0}overflow";

static SEEN: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// 原因 `cause` を初めて見たなら true（ログに出してよい）。2 回目以降と、上限を超えた後は false。
pub fn first(cause: &str) -> bool {
    first_in(&mut SEEN.lock(), cause)
}

/// 覚えている原因を全部忘れる
pub fn reset() {
    SEEN.lock().clear();
}

fn first_in(seen: &mut BTreeSet<String>, cause: &str) -> bool {
    if seen.contains(cause) {
        return false;
    }
    if seen.len() >= MAX_CAUSES {
        if seen.insert(OVERFLOW.to_string()) {
            tracing::warn!("MotionBlur_H: 失敗の原因が {MAX_CAUSES} 種類を超えたので、以降は新しい原因もログに出しません");
        }
        return false;
    }
    seen.insert(cause.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_first_time_per_cause() {
        let mut seen = BTreeSet::new();
        assert!(first_in(&mut seen, "a"));
        assert!(!first_in(&mut seen, "a"));
        assert!(first_in(&mut seen, "b"));
        assert!(!first_in(&mut seen, "b"));
        assert!(!first_in(&mut seen, "a"));
    }

    #[test]
    fn stops_at_the_cap() {
        let mut seen = BTreeSet::new();
        for i in 0..MAX_CAUSES {
            assert!(first_in(&mut seen, &format!("cause{i}")));
        }
        assert!(!first_in(&mut seen, "one more"));
        assert!(!first_in(&mut seen, "and another"));
        // 上限の印は 1 つだけ足される
        assert_eq!(seen.len(), MAX_CAUSES + 1);
    }

    #[test]
    fn reset_forgets() {
        // 共有の SEEN を使うのはこのテストだけ（他のテストと並行しても干渉しないキーにする）
        let key = "log_once::tests::reset_forgets";
        reset();
        assert!(first(key));
        assert!(!first(key));
        reset();
        assert!(first(key));
    }
}
