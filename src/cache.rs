//! 前のフレームの「効果が動かした分」（obj.ox など）を覚えておくキャッシュ。
//!
//! 標準描画とグループ制御の値は、どのフレームのものでも本体からその場で取れる（filter.rs の `live_at`）。
//! 取れないのは、座標フィルタや基準揃え_H のような効果が足した分だけなので、ここにはそれだけを置く。
//!
//! 原作（ObjectMotionBlur_LK）からの変更点:
//! - キーを (効果, 描画対象のオブジェクト, 個別オブジェクトの番号) にした。原作は個別オブジェクトの
//!   番号だけで保存枠を選んでいたため、グループ制御に掛けると配下の全オブジェクトが 1 枠を共有した
//! - 記録には、そのフレームの標準描画とグループ制御の値（`base_hash`）を添える。使うときに今の値と
//!   突き合わせ、合わなければ（移動を直した・グループ制御を足した等）使わない
//! - 同じフレームが違う値で描き直されたら、編集があったとみてそのオブジェクトの記録を全部捨てる。
//!   原作は同じフレームの描き直しで前フレームを入れ替えず、直す前の位置からぶれていた
//! - 古い記録（直前に描いたフレームのものでないもの）は、その後に編集が無かったときだけ使う

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use parking_lot::Mutex;

/// 効果が動かした分。`FILTER_PROC_VIDEO::param` の写し（標準描画からの相対値）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParamPart {
    pub x: f32,
    pub y: f32,
    pub cx: f32,
    pub cy: f32,
    pub rz: f32,
    pub sx: f32,
    pub sy: f32,
}

impl ParamPart {
    pub fn approx_eq(&self, o: &ParamPart) -> bool {
        const TOL: f32 = 1.0e-3;
        let a = [self.x, self.y, self.cx, self.cy, self.rz, self.sx, self.sy];
        let b = [o.x, o.y, o.cx, o.cy, o.rz, o.sx, o.sy];
        a.iter().zip(b.iter()).all(|(p, q)| (p - q).abs() <= TOL)
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Key {
    pub effect_id: i64,
    pub object_id: i64,
    pub index: u32,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    /// オブジェクト基準のフレーム（時間制御があると、シーンのフレームと 1 対 1 にならない）
    local: u32,
    base_hash: u64,
    param: ParamPart,
    generation: u64,
}

#[derive(Default)]
struct ObjCache {
    /// シーン基準のフレーム（origin_frame）ごとの記録
    entries: BTreeMap<u32, Entry>,
    /// 直前に描いたシーン基準のフレーム
    last_frame: Option<u32>,
}

/// `record` の結果。
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordInfo {
    /// 同じフレームの記録が既にあり、値が一致した（描き直しで、何も変わっていない）
    pub matched: bool,
    /// 同じフレームの記録が既にあり、値が違ったので全部捨てた（編集があった）
    pub invalidated: bool,
    /// このオブジェクトを直前に描いたのは 1 つ前のフレームだった（再生・→ キー・出力）
    pub consecutive: bool,
}

/// 記録を使ってよいかの判断に使う。
#[derive(Clone, Copy, Debug)]
pub struct Lookup {
    pub frame: u32,
    pub base_hash: u64,
}

/// 何か編集されるたびに 1 つ進む（`bump_generation`）。古い記録の使用可否に使う。
static GENERATION: AtomicU64 = AtomicU64::new(0);
static CACHE: LazyLock<Mutex<HashMap<Key, ObjCache>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

const MAX_ENTRIES_PER_OBJECT: usize = 4096;
const MAX_OBJECTS: usize = 20000;

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

pub fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn clear_all() {
    CACHE.lock().clear();
    bump_generation();
}

/// 今描いているフレームを記録する。
pub fn record(key: Key, frame: u32, local: u32, base_hash: u64, param: ParamPart) -> RecordInfo {
    let generation = generation();
    let mut map = CACHE.lock();
    if map.len() > MAX_OBJECTS && !map.contains_key(&key) {
        map.clear();
    }
    let cache = map.entry(key).or_default();

    let mut info = RecordInfo {
        consecutive: frame > 0 && cache.last_frame == Some(frame - 1),
        ..Default::default()
    };

    if let Some(old) = cache.entries.get(&frame) {
        if old.base_hash == base_hash && old.local == local && old.param.approx_eq(&param) {
            info.matched = true;
        } else {
            cache.entries.clear();
            info.invalidated = true;
            info.consecutive = false;
        }
    }

    cache.entries.insert(
        frame,
        Entry {
            local,
            base_hash,
            param,
            generation,
        },
    );
    cache.last_frame = Some(frame);

    if cache.entries.len() > MAX_ENTRIES_PER_OBJECT {
        // 今のフレームから最も遠いものを捨てる
        let first = *cache.entries.keys().next().unwrap();
        let last = *cache.entries.keys().next_back().unwrap();
        let drop = if frame.abs_diff(first) > frame.abs_diff(last) { first } else { last };
        cache.entries.remove(&drop);
    }

    info
}

/// 指定フレームの記録を探す。返すのは (そのフレームのオブジェクト基準のフレーム, 効果が動かした分)。
///
/// 使ってよいのは、次の両方を満たすとき。
/// - その記録の `base_hash` が、今その時刻について本体から取った値と一致する
/// - その記録が新しい: 直前に描いたフレームのもの（`info.consecutive` で 1 つ前を引くとき）か、
///   記録してから編集が無いか、今のフレームの描き直しで値が変わっていない（`info.matched`）
pub fn find(key: Key, frame: u32, info: RecordInfo) -> Option<(u32, u64, ParamPart, bool)> {
    let generation = generation();
    let map = CACHE.lock();
    let entry = map.get(&key)?.entries.get(&frame)?;
    let fresh = info.consecutive || info.matched || entry.generation == generation;
    Some((entry.local, entry.base_hash, entry.param, fresh))
}

/// `find` の結果を、今の値と突き合わせて使えるかを決める。
pub fn usable(found: Option<(u32, u64, ParamPart, bool)>, live: Lookup) -> Option<ParamPart> {
    let (_, base_hash, param, fresh) = found?;
    (fresh && base_hash == live.base_hash).then_some(param)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: i64) -> Key {
        Key {
            effect_id: 1,
            object_id: id,
            index: 0,
        }
    }

    fn p(x: f32) -> ParamPart {
        ParamPart {
            x,
            sx: 1.0,
            sy: 1.0,
            ..Default::default()
        }
    }

    // テストは並列に走るので、グローバルを消さず、オブジェクトの ID をテストごとに変えて分ける
    #[test]
    fn objects_do_not_share_entries() {
        // 原作の不具合: グループ制御の配下では全員が同じ枠に書いた
        record(key(100), 10, 0, 7, p(1.0));
        record(key(200), 10, 0, 7, p(2.0));
        let a = find(key(100), 10, RecordInfo::default()).unwrap();
        let b = find(key(200), 10, RecordInfo::default()).unwrap();
        assert_eq!(a.2.x, 1.0);
        assert_eq!(b.2.x, 2.0);
    }

    #[test]
    fn redraw_with_new_value_drops_old_frames() {
        record(key(300), 9, 0, 7, p(0.0));
        record(key(300), 10, 1, 7, p(5.0));
        // 同じフレームを違う値で描き直した（編集）
        let info = record(key(300), 10, 1, 7, p(8.0));
        assert!(info.invalidated);
        assert!(find(key(300), 9, info).is_none());
    }

    #[test]
    fn stale_entry_rejected_after_edit_elsewhere() {
        record(key(400), 20, 0, 7, p(0.0));
        record(key(400), 21, 1, 7, p(1.0));
        bump_generation(); // 別の場所の編集
        // 飛んで 50 を描き、そのあと 21 → 22 と描く。21 の記録は描き直されるので新しい
        record(key(400), 50, 30, 7, p(9.0));
        let info = record(key(400), 22, 2, 7, p(2.0));
        assert!(!info.consecutive);
        let found = find(key(400), 21, info);
        assert!(usable(found, Lookup { frame: 21, base_hash: 7 }).is_none(), "編集後の古い記録を使った");
    }

    #[test]
    fn consecutive_entry_is_used() {
        record(key(500), 30, 0, 7, p(0.0));
        bump_generation();
        let info = record(key(500), 31, 1, 7, p(1.0));
        assert!(info.consecutive);
        let found = find(key(500), 30, info);
        assert_eq!(usable(found, Lookup { frame: 30, base_hash: 7 }).unwrap().x, 0.0);
        // 位置が変わっていたら使わない
        assert!(usable(find(key(500), 30, info), Lookup { frame: 30, base_hash: 8 }).is_none());
    }
}
