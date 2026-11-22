import json
import os

base = os.path.join(os.path.expandvars(r"%LOCALAPPDATA%"), "Temp", "prog test2")
os.makedirs(base, exist_ok=True)
bat = os.path.join(base, "progtest3.bat")
with open(bat, "w", encoding="ascii") as handle:
    handle.write('@echo off\r\necho arg=%1 > "%TEMP%\\progtest_marker3.txt"\r\n')

# 完整路径 + 引号 + 参数：不该走索引，原样交给 shell 执行
value = '"' + bat + '" hello'

path = "target/release/settings.json"
data = json.load(open(path, encoding="utf-8"))
for item in data["items"]:
    if item.get("id") == "new-file":
        item["actions"] = [{"kind": "Run", "value": value, "delay_ms": 0}]
json.dump(data, open(path, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
print(value)
