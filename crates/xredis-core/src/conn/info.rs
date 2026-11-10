//! `INFO keyspace` 的解析和连接信息(P-05、L-10、L-11)。

use std::collections::BTreeMap;

/// 一个库的统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyspaceInfo {
    pub db: u32,
    pub keys: i64,
    pub expires: i64,
    pub avg_ttl: i64,
}

impl KeyspaceInfo {
    pub fn empty(db: u32) -> Self {
        Self {
            db,
            keys: 0,
            expires: 0,
            avg_ttl: 0,
        }
    }
}

/// 连接信息:每个库一条,库数量来自 `CONFIG GET databases`(失败回落 16)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub databases: Vec<KeyspaceInfo>,
}

impl ConnectionInfo {
    /// 用库数量和解析好的 keyspace 拼出每个库一条的列表,没有统计的库补 0。
    pub fn build(db_count: u32, keyspace: &BTreeMap<u32, KeyspaceInfo>) -> Self {
        Self {
            databases: (0..db_count)
                .map(|db| {
                    keyspace
                        .get(&db)
                        .copied()
                        .unwrap_or_else(|| KeyspaceInfo::empty(db))
                })
                .collect(),
        }
    }
}

/// 解析 `INFO keyspace` 的输出。不 panic,不伪造 db0,忽略不认识的字段(L-10)。
pub fn parse_keyspace(info: &str) -> BTreeMap<u32, KeyspaceInfo> {
    info.lines()
        .filter_map(parse_keyspace_line)
        .map(|k| (k.db, k))
        .collect()
}

/// `db0:keys=1,expires=0,avg_ttl=0`;必须严格是 `db<数字>:`,其他以 db 开头的字段不算。
fn parse_keyspace_line(line: &str) -> Option<KeyspaceInfo> {
    let rest = line.trim().strip_prefix("db")?;
    let (db, stats) = rest.split_once(':')?;
    // 只接受纯数字:u32::parse 会放过 "+1" 这类,这里显式要求全是 ASCII 数字
    if db.is_empty() || !db.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let db: u32 = db.parse().ok()?;
    let mut info = KeyspaceInfo::empty(db);
    for stat in stats.split(',') {
        // 畸形项和不认识的新字段(Redis 8 的 subexpiry)一并跳过
        let Some((key, value)) = stat.split_once('=') else {
            continue;
        };
        let Ok(n) = value.parse::<i64>() else {
            continue;
        };
        match key {
            "keys" => info.keys = n,
            "expires" => info.expires = n,
            "avg_ttl" => info.avg_ttl = n,
            _ => {}
        }
    }
    Some(info)
}

/// 解析 `CONFIG GET databases` 的返回(`["databases", "16"]`);取不到或不合法回落默认值(L-11)。
pub fn db_count_or_default(config_get: Option<&[String]>) -> u32 {
    config_get
        .and_then(|v| v.get(1))
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(crate::consts::DEFAULT_DB_COUNT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L-10:Redis 8.4 的真实输出,含新增的 subexpiry,未知字段被忽略。
    #[test]
    fn redis8_line_with_unknown_subexpiry_field_parses() {
        let map = parse_keyspace("# Keyspace\r\ndb0:keys=46,expires=0,avg_ttl=0,subexpiry=0\r\n");
        assert_eq!(map.len(), 1);
        assert_eq!(
            map[&0],
            KeyspaceInfo {
                db: 0,
                keys: 46,
                expires: 0,
                avg_ttl: 0
            }
        );
    }

    #[test]
    fn multiple_dbs_parse() {
        let map = parse_keyspace(
            "# Keyspace\ndb0:keys=1,expires=0,avg_ttl=0\ndb3:keys=7,expires=2,avg_ttl=100\n",
        );
        assert_eq!(map.len(), 2);
        assert_eq!(map[&0].keys, 1);
        assert_eq!(
            map[&3],
            KeyspaceInfo {
                db: 3,
                keys: 7,
                expires: 2,
                avg_ttl: 100
            }
        );
    }

    #[test]
    fn comment_and_blank_lines_ignored() {
        assert!(parse_keyspace("# Keyspace\r\n\r\n").is_empty());
        assert!(parse_keyspace("").is_empty());
    }

    #[test]
    fn missing_stat_fields_default_to_zero() {
        let map = parse_keyspace("db0:keys=5\n");
        assert_eq!((map[&0].keys, map[&0].expires, map[&0].avg_ttl), (5, 0, 0));
    }

    #[test]
    fn line_without_colon_must_not_panic() {
        assert!(parse_keyspace("db0\n").is_empty());
    }

    #[test]
    fn stat_without_equals_must_not_panic() {
        let map = parse_keyspace("db0:keys\n");
        assert!(map.is_empty() || map.get(&0).map(|d| d.keys) == Some(0));
    }

    #[test]
    fn non_ascii_db_name_must_not_forge_db0() {
        assert!(!parse_keyspace("dbé:keys=1,expires=0,avg_ttl=0\n").contains_key(&0));
    }

    #[test]
    fn unparsable_db_index_must_not_forge_db0() {
        assert!(!parse_keyspace("db:keys=99,expires=0,avg_ttl=0\n").contains_key(&0));
        assert!(!parse_keyspace("db+1:keys=99\n").contains_key(&1));
        assert!(!parse_keyspace("db-1:keys=99\n").contains_key(&0));
    }

    #[test]
    fn non_keyspace_line_starting_with_db_must_be_ignored() {
        assert!(parse_keyspace("db_stats:foo=1,bar=2\n").is_empty());
    }

    #[test]
    fn connection_info_fills_missing_dbs_with_zero() {
        let ks = parse_keyspace("db1:keys=2,expires=0,avg_ttl=0\n");
        let info = ConnectionInfo::build(3, &ks);
        assert_eq!(info.databases.len(), 3);
        assert_eq!(info.databases[0], KeyspaceInfo::empty(0));
        assert_eq!(info.databases[1].keys, 2);
        assert_eq!(info.databases[2], KeyspaceInfo::empty(2));
    }

    /// L-11:CONFIG 被禁用(取不到)回落 16。
    #[test]
    fn db_count_falls_back_to_16() {
        let s = |a: &str, b: &str| vec![a.to_string(), b.to_string()];
        assert_eq!(db_count_or_default(None), 16);
        assert_eq!(db_count_or_default(Some(&s("databases", "32"))), 32);
        assert_eq!(db_count_or_default(Some(&s("databases", "0"))), 16);
        assert_eq!(db_count_or_default(Some(&s("databases", "x"))), 16);
        assert_eq!(db_count_or_default(Some(&[])), 16);
    }
}
