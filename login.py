import base64
import configparser
import json
import os
import sys
from html.parser import HTMLParser
from urllib.parse import urljoin, urlsplit, urlunsplit

import requests


class _LoginPageParser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.inputs = {}
        self._in_error_span = False
        self.error_text = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "input":
            name = attrs.get("name") or attrs.get("id")
            if name:
                self.inputs[name] = attrs.get("value", "")
        elif tag == "span" and attrs.get("id") == "errormsghide":
            self._in_error_span = True

    def handle_endtag(self, tag):
        if tag == "span" and self._in_error_span:
            self._in_error_span = False

    def handle_data(self, data):
        if self._in_error_span:
            self.error_text.append(data)

    @property
    def error(self):
        return "".join(self.error_text).strip()


def _read_der_tlv(data, offset=0):
    if offset >= len(data):
        raise ValueError("DER data truncated")
    tag = data[offset]
    offset += 1

    if offset >= len(data):
        raise ValueError("DER length missing")
    first = data[offset]
    offset += 1

    if first & 0x80:
        count = first & 0x7F
        if count == 0 or offset + count > len(data):
            raise ValueError("invalid DER length")
        length = int.from_bytes(data[offset:offset + count], "big")
        offset += count
    else:
        length = first

    end = offset + length
    if end > len(data):
        raise ValueError("DER value truncated")
    return tag, data[offset:end], end


def _parse_rsa_public_key(der_b64):
    der = base64.b64decode(der_b64)
    tag, outer, end = _read_der_tlv(der, 0)
    if tag != 0x30 or end != len(der):
        raise ValueError("invalid SubjectPublicKeyInfo")

    _, _, pos = _read_der_tlv(outer, 0)
    tag, bit_string, pos = _read_der_tlv(outer, pos)
    if tag != 0x03 or not bit_string or bit_string[0] != 0:
        raise ValueError("invalid RSA public key bit string")

    tag, rsa_seq, end = _read_der_tlv(bit_string[1:], 0)
    if tag != 0x30 or end != len(bit_string) - 1:
        raise ValueError("invalid RSAPublicKey sequence")

    tag, n_bytes, pos = _read_der_tlv(rsa_seq, 0)
    if tag != 0x02:
        raise ValueError("RSA modulus missing")
    tag, e_bytes, pos = _read_der_tlv(rsa_seq, pos)
    if tag != 0x02 or pos != len(rsa_seq):
        raise ValueError("RSA exponent missing")

    return int.from_bytes(n_bytes, "big"), int.from_bytes(e_bytes, "big")


def _random_nonzero_bytes(length):
    out = bytearray()
    while len(out) < length:
        chunk = os.urandom(length - len(out))
        out.extend(b for b in chunk if b != 0)
    return bytes(out)


def rsa_pkcs1_v1_5_encrypt(plaintext, public_key_b64):
    modulus, exponent = _parse_rsa_public_key(public_key_b64)
    key_size = (modulus.bit_length() + 7) // 8
    if len(plaintext) > key_size - 11:
        raise ValueError("credential is too long for RSA key")

    padding = _random_nonzero_bytes(key_size - len(plaintext) - 3)
    encoded = b"\x00\x02" + padding + b"\x00" + plaintext
    cipher_int = pow(int.from_bytes(encoded, "big"), exponent, modulus)
    return base64.b64encode(cipher_int.to_bytes(key_size, "big")).decode("ascii")


class IpgwLogin:
    PASS_LOGIN_URL = "https://pass.neu.edu.cn/tpass/login"
    IPGW_PORTAL_URL = "https://ipgw.neu.edu.cn/srun_portal_pc?ac_id=1"
    IPGW_SSO_BASE = "http://ipgw.neu.edu.cn/srun_portal_sso?"
    IPGW_API_BASE = "https://ipgw.neu.edu.cn/v1"
    IPGW_STATUS_URL = "https://ipgw.neu.edu.cn/cgi-bin/rad_user_info"

    # Current RSA public key used by NEU unified authentication.
    # This matches the current NEU_IPGW implementation (v0.2.2+).
    RSA_PUBLIC_KEY_B64 = (
        "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAnjA28DLKXZzxbKmo9/1WkVLf"
        "1mr+wtLXLXt6sC4WiBCtsbzF5ewm7ARZeAdS3iZtqlYPn6IcUoOw42H8nAK/tfFcIb6d"
        "Z1K0atn0U39oWCGPzYuKtLJeMuNZiDXVuAXtojrckOjLW9B3gUnaNGLuIx0fYe66l0o9"
        "WjU2cGLNZQfiIxs2h00z1EA9IdSnVxiVQWSD+lsP3JZXh2TT287la4Y4603SQNKTK/Qv"
        "XfcmccwTEd1IW6HwGxD6QrkInBiHisKWxmveN7UDSaQRZ/J97G0YC32pD38WT53izXeK"
        "0p/kU/X37VP555um1wVWFvPIuc9I7gMP1+hq5a+X6c++tQIDAQAB"
    )

    def __init__(self, session=None, timeout=12):
        self.stu_ID = ""
        self.stu_password = ""
        self.timeout = timeout
        self.session = session or requests.Session()
        self.session.headers.update(
            {
                "User-Agent": (
                    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
                    "(KHTML, like Gecko) Chrome/124.0 Safari/537.36"
                )
            }
        )

    def config(self, Fname="config.ini"):
        filepath = os.path.dirname(os.path.realpath(__file__))
        filename = os.path.join(filepath, Fname)

        conf = configparser.ConfigParser()
        loaded = conf.read(filename, encoding="utf-8")
        if not loaded:
            raise FileNotFoundError(f"配置文件不存在：{filename}")
        if not conf.has_section("info"):
            raise ValueError("config.ini 缺少 [info] 段")

        self.stu_ID = conf.get("info", "StudentID", fallback="").strip()
        self.stu_password = conf.get("info", "password", fallback="")
        if not self.stu_ID or not self.stu_password:
            raise ValueError("请在 config.ini 中填写 StudentID 和 password")

    @staticmethod
    def _parse_login_page(text):
        parser = _LoginPageParser()
        parser.feed(text)
        return parser.inputs.get("lt", ""), parser.inputs.get("execution", ""), parser.error

    def _sso_login(self):
        # CAS 登录必须绑定 service。当前 pass.neu.edu.cn 对“不带 service 的 POST”
        # 会直接返回 500，因此先从 IPGW 门户取得当前网络参数，再构造 service。
        try:
            portal = self.session.get(
                self.IPGW_PORTAL_URL,
                timeout=self.timeout,
                allow_redirects=True,
            )
            portal.raise_for_status()
            portal_query = urlsplit(portal.url).query or "ac_id=1"
            service_url = self.IPGW_SSO_BASE + portal_query

            page = self.session.get(
                self.PASS_LOGIN_URL,
                params={"service": service_url},
                timeout=self.timeout,
            )
            page.raise_for_status()
        except requests.RequestException as exc:
            return False, f"访问统一身份认证失败：{exc}"

        lt, execution, _ = self._parse_login_page(page.text)
        if not lt or not execution:
            return False, "统一身份认证页面中未找到 lt/execution，页面结构可能再次变化"

        try:
            rsa_value = rsa_pkcs1_v1_5_encrypt(
                (self.stu_ID + self.stu_password).encode("utf-8"),
                self.RSA_PUBLIC_KEY_B64,
            )
        except (ValueError, TypeError) as exc:
            return False, f"生成 RSA 登录参数失败：{exc}"

        data = {
            "rsa": rsa_value,
            "ul": len(self.stu_ID.encode("utf-8")),
            "pl": len(self.stu_password.encode("utf-8")),
            "lt": lt,
            "execution": execution,
            "_eventId": "submit",
            # 当前登录页还会随表单提交这三个隐藏字段；保持与浏览器行为一致。
            "t_un": "",
            "t_pd": "",
            "t_c": "",
        }

        try:
            response = self.session.post(
                self.PASS_LOGIN_URL,
                params={"service": service_url},
                data=data,
                headers={"Referer": page.url},
                timeout=self.timeout,
                allow_redirects=False,
            )
        except requests.RequestException as exc:
            return False, f"提交统一身份认证失败：{exc}"

        # 正确账号通常返回 302，由 CAS 携带 ticket 跳转回 service。
        if response.status_code in (301, 302, 303, 307, 308):
            return True, None

        if response.status_code >= 400:
            return False, (
                f"提交统一身份认证失败：HTTP {response.status_code}，"
                f"返回：{response.text.strip()[:200]}"
            )

        _, _, error_text = self._parse_login_page(response.text)
        if error_text:
            return False, f"统一身份认证失败：{error_text}"

        return False, f"统一身份认证未返回预期跳转，HTTP {response.status_code}"

    @staticmethod
    def _find_sso_ticket_url(response):
        candidates = []
        for item in response.history:
            location = item.headers.get("Location")
            if location:
                candidates.append(urljoin(item.url, location))
        candidates.append(response.url)

        for candidate in reversed(candidates):
            parsed = urlsplit(candidate)
            if (
                parsed.hostname == "ipgw.neu.edu.cn"
                and parsed.path.endswith("/srun_portal_sso")
                and "ticket=" in parsed.query
            ):
                return candidate
        return None

    def _ipgw_login(self):
        try:
            portal = self.session.get(
                self.IPGW_PORTAL_URL,
                timeout=self.timeout,
                allow_redirects=True,
            )
            portal.raise_for_status()
        except requests.RequestException as exc:
            return False, f"访问校园网门户失败：{exc}"

        portal_query = urlsplit(portal.url).query or "ac_id=1"
        service_url = self.IPGW_SSO_BASE + portal_query

        try:
            ticket_response = self.session.get(
                self.PASS_LOGIN_URL,
                params={"service": service_url},
                timeout=self.timeout,
                allow_redirects=True,
            )
            ticket_response.raise_for_status()
        except requests.RequestException as exc:
            return False, f"获取校园网 SSO ticket 失败：{exc}"

        ticket_url = self._find_sso_ticket_url(ticket_response)
        if not ticket_url:
            return False, "未从统一身份认证跳转中取得校园网 ticket，认证流程可能再次变化"

        parsed = urlsplit(ticket_url)
        api_url = urlunsplit(
            (
                "https",
                "ipgw.neu.edu.cn",
                "/v1" + parsed.path,
                parsed.query,
                "",
            )
        )

        try:
            response = self.session.get(
                api_url,
                headers={"Referer": self.IPGW_PORTAL_URL},
                timeout=self.timeout,
            )
            response.raise_for_status()
        except requests.RequestException as exc:
            return False, f"提交校园网 ticket 失败：{exc}"

        try:
            payload = response.json()
        except ValueError:
            text = response.text.strip()
            if "success" in text:
                return True, "登录成功"
            return False, f"校园网返回了无法识别的内容：{text[:200]}"

        message = str(payload.get("message", ""))
        code = payload.get("code")
        if message == "success":
            return True, "登录成功"
        if message == "ip_already_online_error":
            return True, "当前 IP 已在线"
        if code == 0 and message:
            return True, message
        return False, f"校园网认证失败：{message or payload}"

    def online_status(self):
        try:
            response = self.session.get(
                self.IPGW_STATUS_URL,
                params={"callback": "json"},
                headers={"Referer": "https://ipgw.neu.edu.cn/srun_portal_success?ac_id=1"},
                timeout=self.timeout,
            )
            response.raise_for_status()
        except requests.RequestException:
            return False, None

        text = response.text.strip()
        if "(" in text and text.endswith(")"):
            text = text[text.find("(") + 1:-1]

        try:
            payload = json.loads(text)
        except ValueError:
            return False, None

        if payload.get("error") == "ok":
            return True, payload
        return False, payload

    def login(self):
        online, info = self.online_status()
        if online:
            username = info.get("user_name") if isinstance(info, dict) else ""
            suffix = f"（{username}）" if username else ""
            return True, f"当前已经在线{suffix}"

        ok, message = self._sso_login()
        if not ok:
            return False, message
        return self._ipgw_login()


# Keep the old class name for existing automation/import compatibility.
Ipgw_login = IpgwLogin


def main():
    client = IpgwLogin()
    try:
        client.config()
    except (OSError, ValueError, configparser.Error) as exc:
        print(exc)
        return 2

    ok, message = client.login()
    print(message)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
