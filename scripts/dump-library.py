"""列出 samples/library 的素材清单（用于真实 IM 验证时挑素材）。"""

import sqlite3
import sys

db = sys.argv[1] if len(sys.argv) > 1 else "samples/library/meta.db"
conn = sqlite3.connect(db)
tables = [r[0] for r in conn.execute("select name from sqlite_master where type='table'")]
print("tables:", tables)
cursor = conn.execute("select * from assets")
print("columns:", [d[0] for d in cursor.description])
for row in cursor:
    print(row)
