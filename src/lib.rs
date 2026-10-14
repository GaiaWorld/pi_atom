//! 默认模式是全局线程安全的字符串驻留池；rc 模式是线程局部的单线程驻留池。
//! 同一池中相同的存活字符串共享数据。池只保存弱引用：最后一个 Atom 释放后
//! 字符串数据即释放，过期索引由再次创建或 collect 清理。
//! 按哈希查询只能提供候选；发生碰撞时须用完整字符串确认身份。

#[cfg(feature = "serde")]
extern crate serde;

use core::fmt;
use std::borrow::{Borrow, Cow};
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::iter::*;
use std::ops::Deref;
use std::str::FromStr;
#[cfg(not(feature = "rc"))]
use std::sync::LazyLock;

use fory::{Error as ForyError, ReadContext, Serializer as ForySerializer, WriteContext};
#[cfg(not(feature = "rc"))]
use papaya::{Compute, HashMap as PapayaMap, Operation};
use pi_share::{Share, ShareU32, ShareWeak};

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use smol_str::SmolStr;

#[cfg(not(feature = "rc"))]
static ATOM_MAP: LazyLock<
    PapayaMap<SmolStr, ShareWeak<(SmolStr, Usize)>, rapidhash::fast::RandomState>,
> = LazyLock::new(|| PapayaMap::with_hasher(rapidhash::fast::RandomState::default()));
#[cfg(not(feature = "rc"))]
static HASH_MAP: LazyLock<PapayaMap<Usize, HashEntry, rapidhash::fast::RandomState>> =
    LazyLock::new(|| PapayaMap::with_hasher(rapidhash::fast::RandomState::default()));
/// 驻留的空字符串，首次访问时创建。
#[cfg(not(feature = "rc"))]
pub static EMPTY: LazyLock<Atom> = LazyLock::new(|| Atom::from(""));

#[cfg(feature = "rc")]
thread_local! {
    static ATOM_MAP: std::cell::RefCell<std::collections::HashMap<SmolStr, ShareWeak<(SmolStr, Usize)>, rapidhash::fast::RandomState>> =
        std::cell::RefCell::new(std::collections::HashMap::with_hasher(rapidhash::fast::RandomState::default()));
    static HASH_MAP: std::cell::RefCell<std::collections::HashMap<Usize, HashEntry, rapidhash::fast::RandomState>> =
        std::cell::RefCell::new(std::collections::HashMap::with_hasher(rapidhash::fast::RandomState::default()));
    pub static EMPTY: Atom = Atom::from("");
}

struct HashEntry {
    weak: ShareWeak<(SmolStr, Usize)>,
    // 只保留一个候选，无法追踪其他碰撞对象的析构；计数只能单调饱和。
    collisions: ShareU32,
}

/// RapidHash 哈希器类型；字符串指纹由固定版本的 RapidHash V3 计算。
pub type CurHasher = rapidhash::fast::RapidHasher<'static>;

#[cfg(feature = "pointer_width_32")]
/// 开启 pointer_width_32 时使用 32 位指纹类型。
pub type Usize = u32;
#[cfg(not(feature = "pointer_width_32"))]
/// 默认使用 64 位指纹类型。
pub type Usize = u64;

/// 驻留字符串。克隆共享数据，按字符串内容比较；rc 模式下不能跨线程使用。
#[derive(Default, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Atom {
    inner: Share<(SmolStr, Usize)>,
}

impl ForySerializer for Atom {
    type Target = Self;

    fn write_data(value: &Self, context: &mut WriteContext<'_>) -> Result<(), ForyError> {
        // 只编码字符串内容，解码时重新驻留，不编码进程内指针或缓存的哈希值。
        <String as ForySerializer>::write_data(&value.as_str().to_owned(), context)
    }

    fn read_data(context: &mut ReadContext<'_>) -> Result<Self, ForyError> {
        Ok(Self::from(<String as ForySerializer>::read_data(context)?))
    }
}

impl Atom {
    /// 从字符串创建或复用一个存活的 Atom。
    pub fn new<T>(text: T) -> Self
    where
        T: AsRef<str>,
    {
        Self::create(SmolStr::new(text))
    }
    /// 从 SmolStr 创建或复用一个存活的 Atom。
    pub fn create(s: SmolStr) -> Atom {
        #[cfg(feature = "rc")]
        return ATOM_MAP.with(|map| {
            if let Some(inner) = map.borrow().get(&s).and_then(ShareWeak::upgrade) {
                return Atom { inner };
            }
            let atom = Atom {
                inner: Share::new((s.clone(), str_hash(&s))),
            };
            map.borrow_mut().insert(s, Share::downgrade(&atom.inner));
            #[cfg(feature = "lookup_by_hash")]
            store_weak(&atom);
            atom
        });

        #[cfg(not(feature = "rc"))]
        {
            let candidate = Share::new((s.clone(), str_hash(&s)));
            let map = ATOM_MAP.pin();
            let weak = Share::downgrade(&candidate);
            // compute 可重试，回调只计算操作，不直接修改共享状态。
            let atom = match map.compute(s, |entry| {
                match entry.and_then(|(_, weak)| weak.upgrade()) {
                    Some(value) => Operation::Abort(value),
                    None => Operation::Insert(weak.clone()),
                }
            }) {
                Compute::Aborted(value) => Atom { inner: value },
                Compute::Inserted(..) | Compute::Updated { .. } => Atom { inner: candidate },
                _ => unreachable!("intern only inserts or reuses"),
            };
            #[cfg(feature = "lookup_by_hash")]
            {
                store_weak(&atom);
            }
            atom
        }
    }

    #[inline(always)]
    /// 借用字符串内容。
    pub fn as_str(&self) -> &str {
        self.inner.0.as_str()
    }
    /// 获取字符串的 RapidHash V3 指纹；碰撞时不能用它唯一标识字符串。
    #[inline(always)]
    pub fn str_hash(&self) -> Usize {
        self.inner.1
    }
}

impl Hash for Atom {
    fn hash<H: Hasher>(&self, h: &mut H) {
        #[cfg(feature = "pointer_width_32")]
        h.write_u32(self.inner.1);
        #[cfg(not(feature = "pointer_width_32"))]
        h.write_u64(self.inner.1)
    }
}

impl Deref for Atom {
    type Target = str;

    fn deref(&self) -> &str {
        self.inner.0.as_str()
    }
}

impl AsRef<str> for Atom {
    #[inline(always)]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl FromIterator<char> for Atom {
    fn from_iter<I: IntoIterator<Item = char>>(iter: I) -> Atom {
        Self::create(SmolStr::from_iter(iter))
    }
}

impl FromIterator<String> for Atom {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Atom {
        Self::create(SmolStr::from_iter(iter))
    }
}

impl<'a> FromIterator<&'a String> for Atom {
    fn from_iter<I: IntoIterator<Item = &'a String>>(iter: I) -> Atom {
        Self::create(SmolStr::from_iter(iter))
    }
}

impl<'a> FromIterator<&'a str> for Atom {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Atom {
        Self::create(SmolStr::from_iter(iter))
    }
}

impl From<&str> for Atom {
    #[inline]
    fn from(s: &str) -> Atom {
        Atom::new(s)
    }
}

impl From<&mut str> for Atom {
    #[inline]
    fn from(s: &mut str) -> Atom {
        Atom::new(s)
    }
}

impl From<&String> for Atom {
    #[inline]
    fn from(s: &String) -> Atom {
        Atom::new(s)
    }
}

impl From<String> for Atom {
    #[inline(always)]
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<Box<str>> for Atom {
    #[inline]
    fn from(s: Box<str>) -> Atom {
        Atom::new(s)
    }
}

impl<'a> From<Cow<'a, str>> for Atom {
    #[inline]
    fn from(s: Cow<'a, str>) -> Atom {
        Atom::new(s)
    }
}
impl<'a> From<&'a [u8]> for Atom {
    #[inline(always)]
    fn from(s: &[u8]) -> Atom {
        Atom::new(core::str::from_utf8(s).unwrap())
    }
}

impl From<Atom> for String {
    #[inline(always)]
    fn from(text: Atom) -> Self {
        text.as_str().into()
    }
}

impl Borrow<str> for Atom {
    #[inline(always)]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for Atom {
    type Err = Infallible;

    #[inline]
    fn from_str(s: &str) -> Result<Atom, Self::Err> {
        Ok(Atom::from(s))
    }
}

#[cfg(feature = "serde")]
impl Serialize for Atom {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_str().serialize(serializer)
    }
}
#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for Atom {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::create(SmolStr::deserialize(deserializer)?))
    }
}

#[inline(always)]
/// 计算固定版本 RapidHash V3 字符串指纹，不保证不同字符串没有碰撞。
pub fn str_hash<R: AsRef<str>>(s: R) -> Usize {
    rapidhash::v3::rapidhash_v3(s.as_ref().as_bytes()) as Usize
}

/// 按哈希查找的三种结果；碰撞计数是饱和观测次数，不是当前存活碰撞数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HashLookup {
    /// 无记录或候选已经释放。
    Missing,
    /// 返回尚未观测到碰撞的候选。
    Found(Atom),
    /// 发生过碰撞；atom 是当前候选，必须用完整字符串确认身份。
    Collision { count: u32, atom: Atom },
}

#[inline(always)]
/// 根据指纹查找候选；碰撞时不会把候选当作唯一匹配。
pub fn get_by_hash(hash: Usize) -> HashLookup {
    #[cfg(feature = "rc")]
    return HASH_MAP.with(|map| {
        let map = map.borrow();
        let Some(entry) = map.get(&hash) else {
            return HashLookup::Missing;
        };
        let Some(inner) = entry.weak.upgrade() else {
            return HashLookup::Missing;
        };
        let count = entry.collisions.load(std::sync::atomic::Ordering::Relaxed);
        if count != 0 {
            HashLookup::Collision {
                count,
                atom: Atom { inner },
            }
        } else {
            HashLookup::Found(Atom { inner })
        }
    });

    #[cfg(not(feature = "rc"))]
    {
        let map = HASH_MAP.pin();
        let Some(entry) = map.get(&hash) else {
            return HashLookup::Missing;
        };
        let Some(atom) = entry.weak.upgrade() else {
            return HashLookup::Missing;
        };
        let count = entry.collisions.load(std::sync::atomic::Ordering::Relaxed);
        if count != 0 {
            return HashLookup::Collision {
                count,
                atom: Atom { inner: atom },
            };
        }
        HashLookup::Found(Atom { inner: atom })
    }
}
/// 查询饱和碰撞观测次数，缺少记录时返回零。
pub fn hash_collisions(hash: Usize) -> u32 {
    #[cfg(feature = "rc")]
    return HASH_MAP.with(|map| {
        map.borrow().get(&hash).map_or(0, |entry| {
            entry.collisions.load(std::sync::atomic::Ordering::Relaxed)
        })
    });
    #[cfg(not(feature = "rc"))]
    HASH_MAP.pin().get(&hash).map_or(0, |entry| {
        entry.collisions.load(std::sync::atomic::Ordering::Relaxed)
    })
}
#[inline(always)]
/// 登记一个 Atom 为哈希查找候选；传入值在调用后释放。
pub fn store_weak_by_hash(atom: Atom) {
    store_weak(&atom);
}
fn store_weak(atom: &Atom) {
    #[cfg(feature = "rc")]
    return HASH_MAP.with(|map| {
        use std::collections::hash_map::Entry;
        let mut map = map.borrow_mut();
        match map.entry(atom.inner.1) {
            Entry::Vacant(entry) => {
                entry.insert(HashEntry {
                    weak: Share::downgrade(&atom.inner),
                    collisions: ShareU32::new(0),
                });
            }
            Entry::Occupied(mut entry) => {
                let current = entry.get();
                let count = current
                    .collisions
                    .load(std::sync::atomic::Ordering::Relaxed);
                match current.weak.upgrade() {
                    Some(value) if Share::ptr_eq(&value, &atom.inner) => {}
                    Some(_) => entry.get_mut().collisions.store(
                        count.saturating_add(1),
                        std::sync::atomic::Ordering::Relaxed,
                    ),
                    None => {
                        entry.insert(HashEntry {
                            weak: Share::downgrade(&atom.inner),
                            collisions: ShareU32::new(count),
                        });
                    }
                }
            }
        }
    });

    #[cfg(not(feature = "rc"))]
    {
        let map = HASH_MAP.pin();
        let weak = Share::downgrade(&atom.inner);
        // 碰撞更新随 compute 的 CAS 一起提交，避免回调重试导致重复计数。
        let _ = map.compute(atom.inner.1, |entry| {
            let Some((_, current)) = entry else {
                return Operation::Insert(HashEntry {
                    weak: weak.clone(),
                    collisions: ShareU32::new(0),
                });
            };
            let count = current
                .collisions
                .load(std::sync::atomic::Ordering::Relaxed);
            match current.weak.upgrade() {
                Some(value) if Share::ptr_eq(&value, &atom.inner) => Operation::Abort(()),
                Some(_) => Operation::Insert(HashEntry {
                    weak: current.weak.clone(),
                    collisions: ShareU32::new(count.saturating_add(1)),
                }),
                None => Operation::Insert(HashEntry {
                    weak: weak.clone(),
                    collisions: ShareU32::new(count),
                }),
            }
        });
    }
}
#[inline(always)]
/// 清理字符串池中过期的键；碰撞历史非零的哈希记录会保留。
pub fn collect() {
    #[cfg(feature = "rc")]
    {
        ATOM_MAP.with(|map| map.borrow_mut().retain(|_, weak| weak.strong_count() > 0));
        HASH_MAP.with(|map| {
            map.borrow_mut().retain(|_, entry| {
                entry.weak.strong_count() > 0
                    || entry.collisions.load(std::sync::atomic::Ordering::Relaxed) > 0
            });
        });
    }
    #[cfg(not(feature = "rc"))]
    {
        ATOM_MAP.pin().retain(|_, weak| weak.strong_count() > 0);
        HASH_MAP.pin().retain(|_, entry| {
            entry.weak.strong_count() > 0
                || entry.collisions.load(std::sync::atomic::Ordering::Relaxed) > 0
        });
    }
}

#[cfg(test)]
mod tests {
    //use std::{time::Duration, thread};

    use crate::*;
    use std::collections::HashMap;

    #[test]
    fn fory_roundtrip_reinterns_string() {
        let mut fory = fory::Fory::builder().xlang(false).build();
        fory.register_serializer::<Atom>(200).unwrap();
        let atom = Atom::from("fory-roundtrip");
        let bytes = fory.serialize(&atom).unwrap();
        let decoded: Atom = fory.deserialize(&bytes).unwrap();
        assert_eq!(decoded.as_str(), atom.as_str());
        assert!(Share::ptr_eq(&decoded.inner, &atom.inner));
    }

    #[test]
    fn drop_and_recreate_reuses_only_live_pool_entries() {
        let atom = Atom::from("reintern-after-drop");
        let duplicate = Atom::from("reintern-after-drop");
        assert!(Share::ptr_eq(&atom.inner, &duplicate.inner));
        drop(duplicate);
        assert!(Share::ptr_eq(
            &atom.inner,
            &Atom::from("reintern-after-drop").inner
        ));
        drop(atom);
        let fresh = Atom::from("reintern-after-drop");
        assert_eq!(fresh.str_hash(), str_hash(fresh.as_str()));
    }

    #[test]
    fn hash_lookup_retains_one_live_candidate_per_hash() {
        assert_eq!(
            get_by_hash(str_hash("missing-hash-entry")),
            HashLookup::Missing
        );
        let first = Atom::from("hash-collision-first");
        let second = Atom::from("hash-collision-second");
        let hash = first.str_hash();
        assert_ne!(first.as_str(), second.as_str());
        store_weak_by_hash(first.clone());
        assert_eq!(get_by_hash(hash), HashLookup::Found(first.clone()));
        assert_eq!(hash_collisions(hash), 0);
        // Force a collision in the index to verify that a live candidate wins.
        #[cfg(not(feature = "rc"))]
        HASH_MAP.pin().insert(
            hash,
            HashEntry {
                weak: Share::downgrade(&second.inner),
                collisions: ShareU32::new(0),
            },
        );
        #[cfg(feature = "rc")]
        HASH_MAP.with(|map| {
            map.borrow_mut().insert(
                hash,
                HashEntry {
                    weak: Share::downgrade(&second.inner),
                    collisions: ShareU32::new(0),
                },
            );
        });
        store_weak_by_hash(first.clone());
        assert_eq!(hash_collisions(hash), 1);
        assert_eq!(
            get_by_hash(hash),
            HashLookup::Collision {
                count: 1,
                atom: second.clone()
            }
        );
        store_weak_by_hash(second.clone());
        assert_eq!(hash_collisions(hash), 1);
        store_weak_by_hash(first.clone());
        assert_eq!(hash_collisions(hash), 2);
        drop(first);
        collect();
        assert_eq!(hash_collisions(hash), 2);
        assert_eq!(
            get_by_hash(hash),
            HashLookup::Collision {
                count: 2,
                atom: second.clone()
            }
        );
    }

    #[test]
    fn hash_lookup_reports_expired_weak_without_collision() {
        let atom = Atom::from("expired-hash-entry");
        let hash = atom.str_hash();
        store_weak_by_hash(atom.clone());
        drop(atom);
        assert_eq!(get_by_hash(hash), HashLookup::Missing);
    }

    #[cfg(not(feature = "rc"))]
    #[test]
    fn concurrent_creation_shares_one_live_allocation() {
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let start = &start;
                    scope.spawn(move || {
                        start.wait();
                        Atom::from("concurrent-intern-entry")
                    })
                })
                .collect();
            let atoms: Vec<_> = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect();
            for atom in &atoms[1..] {
                assert!(Share::ptr_eq(&atom.inner, &atoms[0].inner));
            }
        });
    }

    #[cfg(not(feature = "rc"))]
    #[test]
    fn concurrent_churn_does_not_split_live_interned_values() {
        let anchor = Atom::from("concurrent-churn-entry");
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let anchor = &anchor;
                scope.spawn(move || {
                    for _ in 0..1_000 {
                        let value = Atom::from("concurrent-churn-entry");
                        assert!(Share::ptr_eq(&value.inner, &anchor.inner));
                    }
                });
            }
        });
        drop(anchor);
        collect();
        assert!(ATOM_MAP
            .pin()
            .get(&SmolStr::new("concurrent-churn-entry"))
            .is_none());
    }

    #[cfg(feature = "rc")]
    #[test]
    fn rc_pool_is_thread_local_and_collects_expired_entries() {
        let main = Atom::from("thread-local-intern-entry");
        let same_thread = Atom::from("thread-local-intern-entry");
        assert!(Share::ptr_eq(&main.inner, &same_thread.inner));
        let main_address = Share::as_ptr(&main.inner) as usize;
        let other_address = std::thread::spawn(|| {
            let other = Atom::from("thread-local-intern-entry");
            let duplicate = Atom::from("thread-local-intern-entry");
            assert!(Share::ptr_eq(&other.inner, &duplicate.inner));
            Share::as_ptr(&other.inner) as usize
        })
        .join()
        .unwrap();
        assert_ne!(main_address, other_address);
        drop(same_thread);
        drop(main);
        collect();
        ATOM_MAP.with(|map| {
            assert!(!map
                .borrow()
                .contains_key(&SmolStr::new("thread-local-intern-entry")))
        });
    }

    #[test]
    fn test_atom1() {
        let at3 = Atom::from("RES_GLTF_ACCESSOR_BUFFER_VIEW:app/scene_res/res/u3d_anim/eff_sz_chouka_daiji/eff_sz_chouka_daiji.gltf#Indices#19");
        let at4 = Atom::from("RES_GLTF_ACCESSOR_BUFFER_VIEW:app/scene_res/res/u3d_anim/eff_sz_chouka_daiji/eff_sz_chouka_daiji.gltf#Indices#34");
        println!("at3:{:?}, at4:{:?}", at3.str_hash(), at4.str_hash())
    }

    #[test]
    fn test_atom() {
        let at3 = Atom::from("afg");
        assert_eq!(at3.as_str(), "afg");

        let mut map = HashMap::new();
        let time = std::time::Instant::now();
        for i in 0..1000000 {
            map.insert(i.to_string(), i);
        }
        println!("insert map time:{:?}", std::time::Instant::now() - time);

        let time = std::time::Instant::now();
        let mut vec1 = vec![];
        for i in 0..1000000 {
            vec1.push(Atom::from(i.to_string()));
        }
        println!("atom from time:{:?}", std::time::Instant::now() - time);

        let time = std::time::Instant::now();
        let mut vec2 = vec![];
        for i in 0..1000000 {
            vec2.push(Atom::from(i.to_string()));
        }
        println!("atom look time:{:?}", std::time::Instant::now() - time);

        let mut arr3 = Vec::new();
        for i in 0..1000 {
            arr3.push(Atom::from(i.to_string()));
        }
        let mut arr4 = Vec::new();
        let time = std::time::Instant::now();
        for i in 0..1000 {
            for _ in 0..1000 {
                arr4.push(Atom::from(arr3[i].as_str()));
            }
        }
        println!("atom1 from time:{:?}", std::time::Instant::now() - time);
        let mut arr5 = Vec::new();
        let time = std::time::Instant::now();
        for i in 0..1000 {
            for _ in 0..1000 {
                arr5.push(Share::new((arr3[i].as_str().to_string(), 5)));
            }
        }
        println!("Share::new time:{:?}", std::time::Instant::now() - time);

        let time = std::time::Instant::now();
        for i in 0..1000 {
            for _ in 0..1000 {
                let _ = arr3[i].as_str();
            }
        }
        println!("to_str time:{:?}", std::time::Instant::now() - time);

        let time = std::time::Instant::now();
        let xx = Share::new(1);
        let w = Share::downgrade(&xx);
        for _ in 0..1000000 {
            let _ = w.upgrade();
        }
        println!("upgrade:{:?}", std::time::Instant::now() - time);

        let time = std::time::Instant::now();
        let xx = Share::new(1);
        //let w = Share::downgrade(&xx);
        for _ in 0..1000 {
            for _ in 0..1000 {
                let _a = xx.clone();
            }
        }
        println!("clone: {:?}", std::time::Instant::now() - time);
    }
    #[test]
    fn test_rng() {
        let _thread = std::thread::spawn(|| {
            rng();
            return;
        });

        // thread.join().unwrap();

        rng();
        return;
    }
    fn rng() {
        let mut vec = vec![];
        for _ in 0..1000000 {
            //thread::sleep(Duration::from_millis(0));
            let mut buf = [0u8; 4];
            getrandom::getrandom(&mut buf).unwrap();
            let r = u32::from_ne_bytes(buf);
            if r % 4 == 0 {
                vec.push(Atom::from(r.to_string()));
            } else if r % 4 == 1 && vec.len() > 0 {
                let c = vec[r as usize % vec.len()].clone();
                vec.push(c);
            } else {
                if vec.len() > 0 {
                    vec.swap_remove(r as usize % vec.len());
                }
            }
        }
    }
}
