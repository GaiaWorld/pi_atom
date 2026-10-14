# pi_atom

默认模式为全局线程安全的字符串驻留池；开启 rc 特性后为线程局部的单线程驻留池。相同池内的存活字符串共享数据，可用作 HashMap 的键。

最后一个 Atom 被释放时，字符串数据随之释放。

## 驻留与哈希查询

默认模式使用 papaya 保存弱引用，表索引用随机种子的 RapidHash，不使用全局互斥锁。rc 模式（使用 `--no-default-features --features rc`）改用线程局部的标准 HashMap 和 pi_share 的 Rc；每个线程分别驻留、查询与 collect，Atom 不能跨线程传递。rc 模式下 EMPTY 也是线程局部值；默认模式下是进程级 LazyLock。最后一个 Atom 释放后，过期的索引键会在同字符串再次创建时替换，或由 collect() 清理。开启 lookup_by_hash 特性后，新建 Atom 会自动登记到哈希索引；也可手动调用 store_weak_by_hash()。str_hash() 返回固定版本 RapidHash V3 的字符串指纹。

哈希索引每个哈希只保存一个弱引用候选和一个 ShareU32 碰撞观测计数。get_by_hash() 返回 HashLookup：Missing 表示没有记录或候选已释放；Found(atom) 表示未观测到碰撞；Collision { count, atom } 表示发生过碰撞，atom 只是当前候选，必须用完整字符串确认身份。计数饱和且不会因对象析构回退，不表示当前存活碰撞数；collect() 保留有碰撞历史的记录。哈希值不能作为唯一字符串身份。

## 序列化与特性

Atom 支持 Fory Serializer，按字符串内容编码，解码后重新驻留。使用前须注册自定义序列化器，例如：

```rust
let mut fory = fory::Fory::builder().xlang(false).build();
fory.register_serializer::<pi_atom::Atom>(200)?;
```

注册 ID 由调用方管理；已有 BON 格式不会自动转换为 Fory 格式。可选 serde 特性保留 serde 字符串编解码；pointer_width_32 将指纹截为 u32，增加碰撞可能性。当前 Fory 1.7.7 在本机 i686 Windows 目标编译失败，已在 x86_64 Windows MSVC 验证。

为了减少不停的创建和放入池的次数，高频单次的Atom，可以在应用层增加一个cache来缓冲Atom，定期检查引用计数来判断是否缓冲。
