"""Local IMAP + SMTP test server for RustMail.

IMAP (Twisted, no TLS) on 127.0.0.1:1143, SMTP (aiosmtpd, AUTH PLAIN/LOGIN/
XOAUTH2) on 127.0.0.1:2525. Mail sent over SMTP to the test user is delivered
into the IMAP INBOX. User: alex@example.test / secret, or XOAUTH2 with the
access token "live-access-token" (like Gmail: a bad token gets an error
challenge that the client must answer with an empty line).
"""
import email.utils
import io
import sys
import time
from email.parser import BytesHeaderParser

from twisted.cred import checkers, error, portal
from twisted.internet import defer, protocol, reactor
from twisted.mail import imap4
from twisted.mail.interfaces import IChallengeResponse
from zope.interface import Interface, implementer

USER = b"alex@example.test"
PASSWORD = b"secret"
ACCESS_TOKEN = b"live-access-token"
# Shaped like Gmail's error (kept short: Twisted wraps long base64 lines).
GMAIL_ERROR = b'{"status":"401","schemes":"Bearer"}'


def parse_xoauth2(data):
    """user=...\x01auth=Bearer ...\x01\x01 -> (user, token)"""
    user = token = None
    for part in data.split(b"\x01"):
        if part.startswith(b"user="):
            user = part[5:]
        elif part.startswith(b"auth=Bearer "):
            token = part[12:]
    return user, token


class IXOAuth2(Interface):
    pass


@implementer(IChallengeResponse, IXOAuth2)
class XOAuth2Challenge:
    def __init__(self):
        self.round = 0
        self.ok = False
        self.username = None

    def getChallenge(self):
        return b"" if self.round == 0 else GMAIL_ERROR

    def setResponse(self, response):
        self.round += 1
        if self.round == 1:
            user, token = parse_xoauth2(response)
            self.username = user
            self.ok = user == USER and token == ACCESS_TOKEN
            print(f"IMAP XOAUTH2 for {user!r}: {'ok' if self.ok else 'rejected'}", flush=True)
        elif response:
            print(f"IMAP XOAUTH2: expected an empty reply to the error, got {response!r}", flush=True)
            self.ok = False

    def moreChallenges(self):
        # Gmail sends the error as a challenge and waits for an empty reply.
        return not self.ok and self.round == 1


@implementer(checkers.ICredentialsChecker)
class XOAuth2Checker:
    credentialInterfaces = (IXOAuth2,)

    def requestAvatarId(self, credentials):
        if credentials.ok:
            return defer.succeed(credentials.username)
        return defer.fail(error.UnauthorizedLogin())


@implementer(imap4.IMessage, imap4.IMessageFile)
class Message:
    def __init__(self, uid, data, flags, date):
        self.uid = uid
        self.data = data
        self.flags = set(flags or [])
        self.date = date or email.utils.formatdate(localtime=True)

    def getUID(self):
        return self.uid

    def getFlags(self):
        return list(self.flags)

    def getInternalDate(self):
        return self.date

    def _split(self):
        idx = self.data.find(b"\r\n\r\n")
        if idx < 0:
            idx = self.data.find(b"\n\n")
            return self.data[:idx], self.data[idx + 2:]
        return self.data[:idx], self.data[idx + 4:]

    def getHeaders(self, negate, *names):
        msg = BytesHeaderParser().parsebytes(self.data)
        wanted = {n.lower() for n in names}
        out = {}
        for k, v in msg.items():
            lk = k.lower()
            if not names or (negate and lk not in wanted) or (not negate and lk in wanted):
                out[k] = str(v)
        return out

    def getBodyFile(self):
        return io.BytesIO(self._split()[1])

    def getSize(self):
        return len(self.data)

    def isMultipart(self):
        return False

    def getSubPart(self, part):
        raise IndexError(part)

    def open(self):
        return io.BytesIO(self.data)


@implementer(imap4.IMailbox, imap4.IMessageCopier, imap4.ICloseableMailbox)
class Mailbox:
    def __init__(self, name, uidvalidity):
        self.name = name
        self.msgs = []
        self.uidvalidity = uidvalidity
        self.uidnext = 1
        self.listeners = []

    def getFlags(self):
        return ["\\Seen", "\\Answered", "\\Flagged", "\\Deleted", "\\Draft"]

    def getUIDValidity(self):
        return self.uidvalidity

    def getUIDNext(self):
        return self.uidnext

    def getUID(self, seq):
        return self.msgs[seq - 1].uid

    def getMessageCount(self):
        return len(self.msgs)

    def getRecentCount(self):
        return 0

    def getUnseenCount(self):
        return sum(1 for m in self.msgs if "\\Seen" not in m.flags)

    def isWriteable(self):
        return True

    def destroy(self):
        pass

    def close(self):
        pass

    def getHierarchicalDelimiter(self):
        return "/"

    def requestStatus(self, names):
        r = {}
        for n in names:
            n = n.upper()
            if n == "MESSAGES":
                r[n] = self.getMessageCount()
            elif n == "RECENT":
                r[n] = 0
            elif n == "UIDNEXT":
                r[n] = self.uidnext
            elif n == "UIDVALIDITY":
                r[n] = self.uidvalidity
            elif n == "UNSEEN":
                r[n] = self.getUnseenCount()
        return defer.succeed(r)

    def addListener(self, listener):
        self.listeners.append(listener)

    def removeListener(self, listener):
        if listener in self.listeners:
            self.listeners.remove(listener)

    def deliver(self, data, flags=(), date=None):
        m = Message(self.uidnext, data, flags, date)
        self.uidnext += 1
        self.msgs.append(m)
        for listener in self.listeners:
            try:
                listener.newMessages(len(self.msgs), None)
            except Exception:
                pass
        return m.uid

    def addMessage(self, message, flags=(), date=None):
        data = message.read()
        if isinstance(data, str):
            data = data.encode()
        return defer.succeed(self.deliver(data, [f if isinstance(f, str) else f.decode() for f in flags], date))

    def copy(self, message):
        return self.deliver(message.data, message.getFlags(), message.getInternalDate())

    def expunge(self):
        gone = [m.uid for m in self.msgs if "\\Deleted" in m.flags]
        self.msgs = [m for m in self.msgs if "\\Deleted" not in m.flags]
        return gone

    def _matching(self, messages, uid):
        if not self.msgs:
            return []
        messages.last = self.msgs[-1].uid if uid else len(self.msgs)
        wanted = set(messages)
        out = []
        for i, m in enumerate(self.msgs, 1):
            if (m.uid if uid else i) in wanted:
                out.append((i, m))
        return out

    def fetch(self, messages, uid):
        return self._matching(messages, uid)

    def store(self, messages, flags, mode, uid):
        res = {}
        for i, m in self._matching(messages, uid):
            if mode == 1:
                m.flags.update(flags)
            elif mode == -1:
                m.flags.difference_update(flags)
            else:
                m.flags = set(flags)
            res[i] = m.getFlags()
        return res


class Account(imap4.MemoryAccount):
    def _emptyMailbox(self, name, id):
        return Mailbox(name, 1000 + id)


ACCOUNT = Account(USER)
for box in ["INBOX", "Sent", "Drafts", "Trash", "Junk", "Archive", "Projects", "Projects/Alpha"]:
    ACCOUNT.create(box)


def sample(i, subject, sender, body, html=None, extra=b""):
    date = email.utils.formatdate(time.time() - (20 - i) * 3600, localtime=True)
    hdr = (
        f"From: {sender}\r\nTo: Alex <alex@example.test>\r\nSubject: {subject}\r\nDate: {date}\r\n"
        f"Message-ID: <seed-{i}@example.test>\r\nMIME-Version: 1.0\r\n"
    ).encode() + extra
    if html:
        return hdr + (
            b"Content-Type: multipart/alternative; boundary=b1\r\n\r\n--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n"
            + body.encode() + b"\r\n--b1\r\nContent-Type: text/html; charset=utf-8\r\n\r\n" + html.encode() + b"\r\n--b1--\r\n"
        )
    return hdr + b"Content-Type: text/plain; charset=utf-8\r\n\r\n" + body.encode() + b"\r\n"


inbox = ACCOUNT.select("INBOX")
for i in range(1, 13):
    inbox.deliver(sample(i, f"Test message {i}", f"Sender {i} <sender{i}@example.org>", f"Hello, this is test message number {i}."))
inbox.deliver(sample(13, "HTML newsletter", "News <news@example.org>", "Plain part", "<h1>Big news</h1><p>Some <b>bold</b> text.</p>",
                     b"List-Unsubscribe: <https://example.org/unsub>\r\n"))
inbox.msgs[0].flags.add("\\Seen")
inbox.msgs[1].flags.add("\\Flagged")
ACCOUNT.select("PROJECTS/ALPHA").deliver(sample(14, "Alpha kickoff", "Pat <pat@example.org>", "Kickoff notes."))


@implementer(portal.IRealm)
class Realm:
    def requestAvatar(self, avatarId, mind, *interfaces):
        if imap4.IAccount in interfaces:
            return imap4.IAccount, ACCOUNT, lambda: None
        raise NotImplementedError()


checker = checkers.InMemoryUsernamePasswordDatabaseDontUse()
checker.addUser(USER, PASSWORD)
PORTAL = portal.Portal(Realm(), [checker, XOAuth2Checker()])


class IMAPFactory(protocol.Factory):
    def buildProtocol(self, addr):
        p = imap4.IMAP4Server({b"XOAUTH2": XOAuth2Challenge})
        p.portal = PORTAL
        p.factory = self
        return p


def run_smtp():
    from aiosmtpd.controller import Controller
    from aiosmtpd.smtp import MISSING, AuthResult, LoginPassword

    class Handler:
        async def handle_DATA(self, server, session, envelope):
            data = envelope.original_content or envelope.content
            if isinstance(data, str):
                data = data.encode()
            rcpts = [r.lower() for r in envelope.rcpt_tos]
            print(f"SMTP: message from {envelope.mail_from} to {rcpts} ({len(data)} bytes)", flush=True)
            if USER.decode() in rcpts:
                reactor.callFromThread(lambda: ACCOUNT.select("INBOX").deliver(data))
            return "250 OK queued"

        async def auth_XOAUTH2(self, server, args):
            import base64
            if len(args) < 2:
                data = await server.challenge_auth("")
                if data is MISSING:
                    return AuthResult(success=False)
            else:
                data = base64.b64decode(args[1])
            user, token = parse_xoauth2(data)
            ok = user == USER and token == ACCESS_TOKEN
            print(f"SMTP XOAUTH2 for {user!r}: {'ok' if ok else 'rejected'}", flush=True)
            if ok:
                return AuthResult(success=True, auth_data=user)
            # Gmail answers a bad token with a 334 error challenge first.
            await server.challenge_auth(base64.b64encode(GMAIL_ERROR).decode(), encode_to_b64=False)
            return AuthResult(success=False, message="535 5.7.8 Username and Password not accepted")

    def auth(server, session, envelope, mechanism, auth_data):
        if isinstance(auth_data, LoginPassword) and auth_data.login == USER and auth_data.password == PASSWORD:
            return AuthResult(success=True)
        return AuthResult(success=False, handled=False)

    c = Controller(Handler(), hostname="127.0.0.1", port=2525, authenticator=auth, auth_require_tls=False)
    c.start()
    return c


if __name__ == "__main__":
    smtp = run_smtp()
    reactor.listenTCP(1143, IMAPFactory(), interface="127.0.0.1")
    print("IMAP on 127.0.0.1:1143, SMTP on 127.0.0.1:2525", flush=True)
    sys.stdout.flush()
    reactor.run()
