//! Real Ethereum Merkle Patricia Trie (in-memory, spec-conformant node encoding and hashing).

use rrx_util::{keccak256, keccak256_h, rlp::Rlp, H256, EMPTY_TRIE_HASH};
pub mod verkle;

type Nibbles = Vec<u8>;

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Empty,
    Leaf { partial: Nibbles, value: Vec<u8> },
    Extension { partial: Nibbles, child: Box<Node> },
    Branch { children: Box<[Node; 16]>, value: Vec<u8> },
}

fn empty_branch() -> Node {
    Node::Branch { children: Box::new(std::array::from_fn(|_| Node::Empty)), value: vec![] }
}

fn leaf(p: Nibbles, v: Vec<u8>) -> Node {
    Node::Leaf { partial: p, value: v }
}

fn wrap_ext(prefix: &[u8], child: Node) -> Node {
    if prefix.is_empty() {
        child
    } else {
        Node::Extension { partial: prefix.to_vec(), child: Box::new(child) }
    }
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

fn key_nibbles(key: &[u8]) -> Nibbles {
    key.iter().flat_map(|b| [*b >> 4, *b & 0x0f]).collect()
}

fn hp_encode(nib: &[u8], is_leaf: bool) -> Vec<u8> {
    let odd = nib.len() % 2 == 1;
    let flag: u8 = if is_leaf { 2 } else { 0 };
    let mut out = Vec::with_capacity(nib.len() / 2 + 1);
    if odd {
        out.push((flag + 1) * 16 + nib[0]);
    } else {
        out.push(flag * 16);
    }
    let mut i = if odd { 1 } else { 0 };
    while i < nib.len() {
        out.push(nib[i] * 16 + nib[i + 1]);
        i += 2;
    }
    out
}

impl Node {
    fn get(&self, nib: &[u8]) -> Option<Vec<u8>> {
        match self {
            Node::Empty => None,
            Node::Leaf { partial, value } => {
                if partial.as_slice() == nib {
                    Some(value.clone())
                } else {
                    None
                }
            }
            Node::Extension { partial, child } => {
                if nib.len() >= partial.len() && nib[..partial.len()] == partial[..] {
                    child.get(&nib[partial.len()..])
                } else {
                    None
                }
            }
            Node::Branch { children, value } => {
                if nib.is_empty() {
                    if value.is_empty() {
                        None
                    } else {
                        Some(value.clone())
                    }
                } else {
                    children[nib[0] as usize].get(&nib[1..])
                }
            }
        }
    }

    fn set_child(&mut self, idx: u8, child: Node) {
        if let Node::Branch { children, .. } = self {
            children[idx as usize] = child;
        }
    }

    fn set_value(&mut self, v: Vec<u8>) {
        if let Node::Branch { value, .. } = self {
            *value = v;
        }
    }

    fn insert(self, nib: &[u8], value: Vec<u8>) -> Node {
        match self {
            Node::Empty => leaf(nib.to_vec(), value),
            Node::Leaf { partial, value: old } => {
                let c = common_prefix(&partial, nib);
                if c == partial.len() && c == nib.len() {
                    return leaf(partial, value); // overwrite
                }
                if c == partial.len() {
                    // old leaf is a strict prefix of the new key
                    let mut b = empty_branch();
                    b.set_value(old);
                    let rest = &nib[c..];
                    let sub = if rest.len() == 1 {
                        leaf(vec![], value)
                    } else {
                        leaf(rest[1..].to_vec(), value)
                    };
                    b.set_child(rest[0], sub);
                    return wrap_ext(&partial[..c], b);
                }
                let mut b = empty_branch();
                let old_rest = partial[c + 1..].to_vec();
                b.set_child(
                    partial[c],
                    if old_rest.is_empty() { leaf(vec![], old) } else { leaf(old_rest, old) },
                );
                if c >= nib.len() {
                    // new key ends exactly at the branch point -> branch value
                    b.set_value(value);
                } else {
                    b.set_child(nib[c], leaf(nib[c + 1..].to_vec(), value));
                }
                wrap_ext(&partial[..c], b)
            }
            Node::Extension { partial, child } => {
                let c = common_prefix(&partial, nib);
                if c == partial.len() {
                    let new_child = child.insert(&nib[c..], value);
                    return wrap_ext(&partial, new_child);
                }
                let mut b = empty_branch();
                let old_rest = partial[c + 1..].to_vec();
                let old_child = *child;
                b.set_child(
                    partial[c],
                    if old_rest.is_empty() {
                        old_child
                    } else {
                        Node::Extension { partial: old_rest, child: Box::new(old_child) }
                    },
                );
                if c >= nib.len() {
                    // new key ends exactly at the branch point -> branch value
                    b.set_value(value);
                } else {
                    b.set_child(nib[c], leaf(nib[c + 1..].to_vec(), value));
                }
                wrap_ext(&partial[..c], b)
            }
            Node::Branch { mut children, value: old } => {
                if nib.is_empty() {
                    return Node::Branch { children, value };
                }
                let idx = nib[0] as usize;
                let child = std::mem::replace(&mut children[idx], Node::Empty);
                children[idx] = child.insert(&nib[1..], value);
                Node::Branch { children, value: old }
            }
        }
    }

    fn remove(self, nib: &[u8]) -> (Node, bool) {
        match self {
            Node::Empty => (Node::Empty, false),
            Node::Leaf { partial, value } => {
                if partial.as_slice() == nib {
                    (Node::Empty, true)
                } else {
                    (leaf(partial, value), false)
                }
            }
            Node::Branch { mut children, value } => {
                // Apply the removal, then re-count entries: canonical MPT keeps a
                // branch on two or more entries (children + value slot), so a
                // branch reduced to a single entry must be collapsed.
                let (mut children, value, removed) = if nib.is_empty() {
                    if value.is_empty() {
                        (children, value, false)
                    } else {
                        (children, vec![], true)
                    }
                } else {
                    let idx = nib[0] as usize;
                    let child = std::mem::replace(&mut children[idx], Node::Empty);
                    let (new_child, removed) = child.remove(&nib[1..]);
                    children[idx] = new_child;
                    (children, value, removed)
                };
                if !removed {
                    return (Node::Branch { children, value }, false);
                }
                let live: Vec<usize> =
                    (0..16).filter(|&i| !matches!(children[i], Node::Empty)).collect();
                if live.is_empty() {
                    if value.is_empty() {
                        return (Node::Empty, true);
                    }
                    // Only the value is left: the branch becomes a leaf whose
                    // path is the accumulated key prefix (empty here).
                    return (leaf(vec![], value), true);
                }
                if live.len() == 1 && value.is_empty() {
                    let i = live[0];
                    let sub = std::mem::replace(&mut children[i], Node::Empty);
                    let merged = match sub {
                        Node::Extension { partial, child } => Node::Extension {
                            partial: [[i as u8].as_slice(), &partial].concat(),
                            child,
                        },
                        Node::Leaf { partial, value: v } => {
                            leaf([[i as u8].as_slice(), &partial].concat(), v)
                        }
                        other => Node::Extension {
                            partial: vec![i as u8],
                            child: Box::new(other),
                        },
                    };
                    return (merged, true);
                }
                (Node::Branch { children, value }, true)
            }
            Node::Extension { partial, child } => {
                if nib.len() < partial.len() || nib[..partial.len()] != partial[..] {
                    return (Node::Extension { partial, child }, false);
                }
                let (new_child, removed) = child.remove(&nib[partial.len()..]);
                if !removed {
                    return (Node::Extension { partial, child: Box::new(new_child) }, false);
                }
                match new_child {
                    Node::Extension { partial: p2, child: c2 } => {
                        (Node::Extension { partial: [partial, p2].concat(), child: c2 }, true)
                    }
                    Node::Leaf { partial: p2, value: v } => {
                        // extension -> leaf is not canonical: merge the paths
                        (leaf([partial, p2].concat(), v), true)
                    }
                    Node::Empty => (Node::Empty, true),
                    other => (Node::Extension { partial, child: Box::new(other) }, true),
                }
            }
        }
    }

    fn child_ref(child: &Node) -> Rlp {
        if matches!(child, Node::Empty) {
            return Rlp::Bytes(vec![]);
        }
        let enc = child.rlp_node().encode();
        if enc.len() >= 32 {
            Rlp::Bytes(keccak256(&enc).to_vec())
        } else {
            child.rlp_node()
        }
    }

    fn rlp_node(&self) -> Rlp {
        match self {
            Node::Empty => Rlp::Bytes(vec![]),
            Node::Leaf { partial, value } => {
                Rlp::list(vec![Rlp::Bytes(hp_encode(partial, true)), Rlp::Bytes(value.clone())])
            }
            Node::Extension { partial, child } => {
                Rlp::list(vec![Rlp::Bytes(hp_encode(partial, false)), Self::child_ref(child)])
            }
            Node::Branch { children, value } => {
                let mut items: Vec<Rlp> = children.iter().map(Self::child_ref).collect();
                items.push(if value.is_empty() {
                    Rlp::Bytes(vec![])
                } else {
                    Rlp::Bytes(value.clone())
                });
                Rlp::list(items)
            }
        }
    }
}

/// A Merkle Patricia Trie. Values are raw byte strings; callers choose their canonical
/// encoding (e.g. RLP-trimmed big-endian integers for storage/account tries).
#[derive(Clone, Debug)]
pub struct Trie {
    root: Node,
}

impl Default for Trie {
    fn default() -> Self {
        Self::new()
    }
}

impl Trie {
    pub fn new() -> Self {
        Trie { root: Node::Empty }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.root, Node::Empty)
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.root.get(&key_nibbles(key))
    }

    /// Insert; empty value deletes the key.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) {
        let nib = key_nibbles(key);
        let root = std::mem::replace(&mut self.root, Node::Empty);
        self.root =
            if value.is_empty() { root.remove(&nib).0 } else { root.insert(&nib, value.to_vec()) };
    }

    pub fn remove(&mut self, key: &[u8]) {
        self.insert(key, &[]);
    }

    pub fn root_hash(&self) -> H256 {
        match &self.root {
            Node::Empty => EMPTY_TRIE_HASH,
            root => {
                let enc = root.rlp_node().encode();
                keccak256_h(&enc)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rrx_util::to_hex;

    fn parse_kv(s: &str) -> Vec<u8> {
        if let Some(h) = s.strip_prefix("0x") {
            rrx_util::from_hex(h).unwrap()
        } else {
            s.as_bytes().to_vec()
        }
    }

    fn apply(t: &mut Trie, k: &str, v: Option<&str>) {
        let key = parse_kv(k);
        match v {
            Some(v) => t.insert(&key, &parse_kv(v)),
            None => t.remove(&key),
        }
    }

    #[test]
    fn empty_trie_root() {
        assert_eq!(Trie::new().root_hash(), EMPTY_TRIE_HASH);
    }

    #[test]
    fn trietest_empty_values() {
        let mut t = Trie::new();
        apply(&mut t, "do", Some("verb"));
        apply(&mut t, "ether", Some("wookiedoo"));
        apply(&mut t, "horse", Some("stallion"));
        apply(&mut t, "shaman", Some("horse"));
        apply(&mut t, "doge", Some("coin"));
        apply(&mut t, "ether", None);
        apply(&mut t, "dog", Some("puppy"));
        apply(&mut t, "shaman", None);
        assert_eq!(
            to_hex(t.root_hash().as_bytes()),
            "5991bb8c6514148a29db676a14ac506cd2cd5775ace63c30a4fe457715e9ac84"
        );
        assert_eq!(t.get(b"dog").as_deref(), Some("puppy".as_bytes()));
        assert_eq!(t.get(b"ether"), None);
    }

    #[test]
    fn trietest_branching_tests() {
        let keys = [
            "0x04110d816c380812a427968ece99b1c963dfbce6", "0x095e7baea6a6c7c4c2dfeb977efac326af552d87",
            "0x0a517d755cebbf66312b30fff713666a9cb917e0", "0x24dd378f51adc67a50e339e8031fe9bd4aafab36",
            "0x293f982d000532a7861ab122bdc4bbfd26bf9030", "0x2cf5732f017b0cf1b1f13a1478e10239716bf6b5",
            "0x31c640b92c21a1f1465c91070b4b3b4d6854195f", "0x37f998764813b136ddf5a754f34063fd03065e36",
            "0x37fa399a749c121f8a15ce77e3d9f9bec8020d7a", "0x4f36659fa632310b6ec438dea4085b522a2dd077",
            "0x62c01474f089b07dae603491675dc5b5748f7049", "0x729af7294be595a0efd7d891c9e51f89c07950c7",
            "0x83e3e5a16d3b696a0314b30b2534804dd5e11197", "0x8703df2417e0d7c59d063caa9583cb10a4d20532",
            "0x8dffcd74e5b5923512916c6a64b502689cfa65e1", "0x95a4d7cccb5204733874fa87285a176fe1e9e240",
            "0x99b2fcba8120bedd048fe79f5262a6690ed38c39", "0xa4202b8b8afd5354e3e40a219bdc17f6001bf2cf",
            "0xa94f5374fce5edbc8e2a8697c15331677e6ebf0b", "0xa9647f4a0a14042d91dc33c0328030a7157c93ae",
            "0xaa6cffe5185732689c18f37a7f86170cb7304c2a", "0xaae4a2e3c51c04606dcb3723456e58f3ed214f45",
            "0xc37a43e940dfb5baf581a0b82b351d48305fc885", "0xd2571607e241ecf590ed94b12d87c94babe36db6",
            "0xf735071cbee190d76b704ce68384fc21e389fbe7",
        ];
        let mut t = Trie::new();
        for k in keys {
            apply(&mut t, k, Some("something"));
        }
        for k in keys {
            apply(&mut t, k, None);
        }
        assert_eq!(
            to_hex(t.root_hash().as_bytes()),
            "56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"
        );
    }

    #[test]
    fn trietest_jeff() {
        let mut t = Trie::new();
        apply(&mut t, "0x0000000000000000000000000000000000000000000000000000000000000045", Some("0x22b224a1420a802ab51d326e29fa98e34c4f24ea"));
        apply(&mut t, "0x0000000000000000000000000000000000000000000000000000000000000046", Some("0x67706c2076330000000000000000000000000000000000000000000000000000"));
        apply(&mut t, "0x0000000000000000000000000000000000000000000000000000001234567890", Some("0x697c7b8c961b56f675d570498424ac8de1a918f6"));
        apply(&mut t, "0x000000000000000000000000697c7b8c961b56f675d570498424ac8de1a918f6", Some("0x1234567890"));
        apply(&mut t, "0x0000000000000000000000007ef9e639e2733cb34e4dfc576d4b23f72db776b2", Some("0x4655474156000000000000000000000000000000000000000000000000000000"));
        apply(&mut t, "0x000000000000000000000000ec4f34c97e43fbb2816cfd95e388353c7181dab1", Some("0x4e616d6552656700000000000000000000000000000000000000000000000000"));
        apply(&mut t, "0x4655474156000000000000000000000000000000000000000000000000000000", Some("0x7ef9e639e2733cb34e4dfc576d4b23f72db776b2"));
        apply(&mut t, "0x4e616d6552656700000000000000000000000000000000000000000000000000", Some("0xec4f34c97e43fbb2816cfd95e388353c7181dab1"));
        apply(&mut t, "0x0000000000000000000000000000000000000000000000000000001234567890", None);
        apply(&mut t, "0x000000000000000000000000697c7b8c961b56f675d570498424ac8de1a918f6", Some("0x6f6f6f6820736f2067726561742c207265616c6c6c793f000000000000000000"));
        apply(&mut t, "0x6f6f6f6820736f2067726561742c207265616c6c6c793f000000000000000000", Some("0x697c7b8c961b56f675d570498424ac8de1a918f6"));
        assert_eq!(
            to_hex(t.root_hash().as_bytes()),
            "9f6221ebb8efe7cff60a716ecb886e67dd042014be444669f0159d8e68b42100"
        );
    }

    #[test]
    fn trietest_insert_middle_leaf() {
        let mut t = Trie::new();
        apply(&mut t, "key1aa", Some("0123456789012345678901234567890123456789xxx"));
        apply(&mut t, "key1", Some("0123456789012345678901234567890123456789Very_Long"));
        apply(&mut t, "key2bb", Some("aval3"));
        apply(&mut t, "key2", Some("short"));
        apply(&mut t, "key3cc", Some("aval3"));
        apply(&mut t, "key3", Some("1234567890123456789012345678901"));
        assert_eq!(
            to_hex(t.root_hash().as_bytes()),
            "cb65032e2f76c48b82b5c24b3db8f670ce73982869d38cd39a624f23d62a9e89"
        );
    }

    #[test]
    fn trietest_branch_value_update() {
        let mut t = Trie::new();
        apply(&mut t, "abc", Some("123"));
        apply(&mut t, "abcd", Some("abcd"));
        apply(&mut t, "abc", Some("abc"));
        assert_eq!(
            to_hex(t.root_hash().as_bytes()),
            "7a320748f780ad9ad5b0837302075ce0eeba6c26e3d8562c67ccc0f1b273298a"
        );
    }

    #[test]
    fn insert_get_overwrite() {
        let mut t = Trie::new();
        t.insert(b"a", b"1");
        t.insert(b"ab", b"2");
        t.insert(b"abc", b"3");
        assert_eq!(t.get(b"a").unwrap(), b"1");
        assert_eq!(t.get(b"ab").unwrap(), b"2");
        t.insert(b"ab", b"9");
        assert_eq!(t.get(b"ab").unwrap(), b"9");
        t.remove(b"ab");
        assert_eq!(t.get(b"ab"), None);
        assert_eq!(t.get(b"a").unwrap(), b"1");
        assert_eq!(t.get(b"abc").unwrap(), b"3");
    }
}
