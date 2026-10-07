import sys
import requests

IPGW_LOGOUT_URL = "https://ipgw.neu.edu.cn/cgi-bin/srun_portal?action=logout&username="
IPGW_STATUS_URL = "https://ipgw.neu.edu.cn/cgi-bin/rad_user_info"
IPGW_SUCCESS_URL = "https://ipgw.neu.edu.cn/srun_portal_success?ac_id=1"


def is_online(session, timeout=10):
    try:
        r = session.get(
            IPGW_STATUS_URL,
            params={"callback": "json"},
            headers={"Referer": IPGW_SUCCESS_URL},
            timeout=timeout,
        )
        r.raise_for_status()
    except requests.RequestException:
        return None

    text = r.text.strip()
    if "(" in text and text.endswith(")"):
        text = text[text.find("(") + 1:-1]

    try:
        data = r.json() if not text else __import__("json").loads(text)
    except ValueError:
        return None

    return data.get("error") == "ok"


def logout(timeout=10):
    session = requests.Session()
    session.headers.update(
        {
            "User-Agent": (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
                "AppleWebKit/537.36 (KHTML, like Gecko) "
                "Chrome/124.0 Safari/537.36"
            )
        }
    )

    online = is_online(session, timeout)
    if online is False:
        return True, "当前已经离线"

    try:
        r = session.get(
            IPGW_LOGOUT_URL,
            headers={"Referer": IPGW_SUCCESS_URL},
            timeout=timeout,
        )
        r.raise_for_status()
    except requests.RequestException as exc:
        return False, f"注销请求失败：{exc}"

    # 再检查一次在线状态，避免只根据 HTTP 200 误判。
    after = is_online(session, timeout)
    if after is False:
        return True, "注销成功"
    if after is True:
        return False, "已发送注销请求，但当前 IP 仍显示在线"

    # 某些网络环境下状态接口可能暂时不可用，此时参考注销接口返回。
    text = r.text.strip().lower()
    if "logout" in text or "success" in text or r.status_code == 200:
        return True, "注销请求已提交"

    return False, f"无法确认注销结果：{r.text.strip()[:200]}"


def main():
    ok, message = logout()
    print(message)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
