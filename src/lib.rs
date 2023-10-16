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
use std::convert::{From, Infallible};
use std::hash::Hash;
use std::iter::*;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::atomic::Ordering;

use dashmap::DashMap;
use pi_key_alloter::new_key_type;
use pi_share::ShareUsize;
use pi_slot::SlotMap;
#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use smol_str::SmolStr;

// 同步原语，可用于运行一次性初始化。用于全局，FFI或相关功能的一次初始化。
lazy_static! {
    static ref SLOT_MAP: SlotMap<Key, (SmolStr, ShareUsize)> = SlotMap::default();
    static ref ATOM_MAP: DashMap<SmolStr, Key> = DashMap::default();
}

// 原子字符串
new_key_type! {
    struct Key;
}
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Atom(Key);

impl Atom {
    pub fn new<T>(text: T) -> Atom
    where
        T: AsRef<str>,
    {
        Self::create(SmolStr::new(text))
    }
    pub fn create(s: SmolStr) -> Atom {
        match ATOM_MAP.entry(s) {
            dashmap::mapref::entry::Entry::Occupied(entry) => {
                let key = *entry.get();
                let (_, n) = SLOT_MAP.get(key).unwrap();
                n.fetch_add(1, Ordering::Release);
                Atom(key)
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let key = SLOT_MAP.insert((entry.key().clone(), ShareUsize::new(1)));
                entry.insert(key);
                Atom(key)
            }
        }
    }
    #[inline(always)]
    pub fn as_str(&self) -> &str {
        SLOT_MAP.get(self.0).unwrap().0.as_str()
    }
    // #[inline(always)]
    // fn from_char_iter<I: Iterator<Item = char>>(iter: I) -> Atom {
    //     Self::create(SmolStr::from_iter(iter))
    // }
}
impl Clone for Atom {
    fn clone(&self) -> Self {
        if let Some((_, n)) = SLOT_MAP.get(self.0) {
            n.fetch_add(1, Ordering::Release);
        }
        Atom(self.0)
    }
}

impl Drop for Atom {
    fn drop(&mut self) {
        if let Some((s, n)) = SLOT_MAP.get(self.0) {
            if n.fetch_sub(1, Ordering::Release) > 1 {
                return;
            }
            ATOM_MAP.remove_if(s, |_, _| {
                // 进入锁后，再次判断是否需要释放
                if n.load(Ordering::Acquire) > 1 {
                    return false;
                }
                SLOT_MAP.remove(self.0);
                true
            });
        }
    }
}

impl Deref for Atom {
    type Target = SmolStr;

    fn deref(&self) -> &SmolStr {
        &SLOT_MAP.get(self.0).unwrap().0
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
        // let iter = iter.into_iter();
        // Self::from_char_iter(iter)
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
//        Atom::from_iter(iter.into_iter().map(|x| x.as_str()))
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
#[cfg(test)]
mod tests {
    //use std::{time::Duration, thread};

    use crate::*;
    use pi_hash::XHashMap;
    use pi_share::Share;

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

        let time = std::time::Instant::now();
        for i in 0..1000 {
            for _ in 0..1000 {
                Share::new((arr3[i].as_str().to_string(), 5));
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

        let _thread = std::thread::spawn(||{
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
                vec.push( Atom::from(r.to_string()));
            }else if r % 4 == 1 && vec.len() > 0 {
                let c = vec[r as usize % vec.len()].clone();
                vec.push(c);
            }else {
                if vec.len() > 0 {
                    vec.swap_remove(r as usize % vec.len());
                }
            }
           
        }
    }
}
