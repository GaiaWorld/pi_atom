/// 全局的线程安全的原子字符串池，减少相同字符串的内存占用，也用于hashmap的键
/// 如果全局该字符串最后一个引用被释放， 则该字符串会释放。
/// 为了减少不停的创建和放入池的次数，高频单次的Atom，可以在应用层增加一个cache来缓冲Atom，定期检查引用计数来判断是否缓冲。

#[macro_use]
extern crate lazy_static;
#[cfg(feature = "serde")]
#[macro_use]
extern crate serde;

use core::fmt;
use std::borrow::{Borrow, Cow};
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::iter::*;
use std::ops::Deref;
use std::str::FromStr;

use pi_bon::{WriteBuffer, ReadBuffer, Encode, Decode, ReadBonErr};
use dashmap::DashMap;
use pi_share::{Share, ShareWeak};

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use smol_str::SmolStr;

// 同步原语，可用于运行一次性初始化。用于全局，FFI或相关功能的一次初始化。
lazy_static! {
    static ref ATOM_MAP: DashMap<SmolStr, Share<(SmolStr, Usize)>> = DashMap::default();
    static ref HASH_MAP: DashMap<Usize, ShareWeak<(SmolStr, Usize)>> = DashMap::default();
    pub static ref EMPTY: Atom = Atom::from("");
}

#[cfg(all(not(feature = "pi_hash/xxhash"), not(feature = "pointer_width_32")))]
pub type CurHasher = fxhash::FxHasher64;

#[cfg(all(not(feature = "pi_hash/xxhash"), feature = "pointer_width_32"))]
pub type CurHasher = fxhash::FxHasher32;

#[cfg(all(feature = "pi_hash/xxhash", not(feature = "pointer_width_32")))]
pub type CurHasher = twox_hash::XxHash64;

#[cfg(all(feature = "pi_hash/xxhash", feature = "pointer_width_32"))]
pub type CurHasher = twox_hash::XxHash32;

#[cfg(feature = "pointer_width_32")]
pub type Usize = u32;
#[cfg(not(feature = "pointer_width_32"))]
pub type Usize = u64;

#[derive(Default, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Atom(Share<(SmolStr, Usize)>);
unsafe impl Sync for Atom {}
unsafe impl Send for Atom {}

impl Encode for Atom{
    fn encode(&self, bb: &mut WriteBuffer){
        (*self.0).0.as_str().to_string().encode(bb);
    }
}

impl Decode for Atom{
    fn decode(bb: &mut ReadBuffer) -> Result<Atom, ReadBonErr>{
        Ok(Atom::from(String::decode(bb)?))
    }
}

impl Atom {
    pub fn new<T>(text: T) -> Self
    where
        T: AsRef<str>,
    {
        Self::create(SmolStr::new(text))
    }
    pub fn create(s: SmolStr) -> Atom {
        match ATOM_MAP.entry(s) {
            dashmap::mapref::entry::Entry::Occupied(entry) => Atom(entry.get().clone()),
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let s = entry.key().clone();
                let str_hash = str_hash(&s);
                let r = Share::new((s, str_hash));
                entry.insert(r.clone());
                #[cfg(feature="lookup_by_hash")]
                {
                    HASH_MAP.insert(str_hash, Share::downgrade(&r));
                }
                Atom(r)
            }
        }
    }

    #[inline(always)]
    pub fn as_str(&self) -> &str {
        self.0 .0.as_str()
    }
    /// 获取该Atom的hash值
    #[inline(always)]
    pub fn str_hash(&self) -> Usize {
        self.0 .1
    }
}

impl Hash for Atom {
    fn hash<H: Hasher>(&self, h: &mut H) {
        #[cfg(feature = "pointer_width_32")]
        h.write_u32(self.0 .1);
        #[cfg(not(feature = "pointer_width_32"))]
        h.write_u64(self.0 .1)
    }
}
impl Drop for Atom {
    fn drop(&mut self) {
        if Share::<(SmolStr, Usize)>::strong_count(&self.0) > 2 {
            return;
        }
        ATOM_MAP.remove_if(&(self.0).0, |_, _| {
            // 进入锁后，再次判断是否需要释放
            if Share::<(SmolStr, Usize)>::strong_count(&self.0) > 2 {
                return false;
            }
            #[cfg(feature="lookup_by_hash")]
            {
                HASH_MAP.remove(&self.0.1);
            }    
            true
        });
    }
}

impl Deref for Atom {
    type Target = str;

    fn deref(&self) -> &str {
        (self.0).0.as_str()
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
pub fn str_hash<R: AsRef<str>>(s: R) -> Usize {
    let hasher = &mut CurHasher::default();
    s.as_ref().hash(hasher);
    hasher.finish() as Usize
}

#[inline(always)]
pub fn get_by_hash(hash: Usize) -> Option<Atom> {
    HASH_MAP
        .get(&hash)
        .map_or(None, |r| r.value().upgrade().map(|r| Atom(r)))
}
#[inline(always)]
pub fn store_weak_by_hash(atom: Atom) {
    HASH_MAP.insert(atom.0 .1, Share::<(SmolStr, Usize)>::downgrade(&atom.0));
}
#[inline(always)]
pub fn collect() {
    HASH_MAP.retain(|_, v| v.strong_count() > 0);
}

#[cfg(test)]
mod tests {
    //use std::{time::Duration, thread};


    use crate::*;
    use pi_hash::XHashMap;

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

        let mut map = XHashMap::default();
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
            let r = unsafe { *(buf.as_ptr() as usize as *mut u32) };
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
