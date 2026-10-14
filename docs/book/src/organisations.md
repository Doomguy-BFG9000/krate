# Organisations

An organisation is a group of people on a Krate hub who send apps to each
other privately. One person uploads an app, a second person approves it,
and then only the organisation's members and the people it sends to can
open it. A copied link alone opens nothing.

Organisations are a pilot, run with a few design partners while it is new.
Everything below is driven from the `krate` command line.

Installing Krate on an organisation's computers, the policy that says which
apps may open there, the record of what opened, company networks, and
release control are in
[Running Krate in an organisation](running-in-an-organisation.md).

## Who can do what

People are known by a GitHub account or a work email. A GitHub account is
the sign-in `krate publish` already uses: if you have published an app, you
are already signed in. A work email is proven by signing in with an email
link or with Google. `krate account login` signs you in. An email account
is never mistaken for a GitHub account with a similar name: `alice` on
GitHub and `alice@corp.example` are two different people to an organisation.

Each person has one role:

| Role | What it is for |
|---|---|
| owner | Everything, including the organisation's root key and making other owners. |
| admin | Invites and removes developers, reviewers and recipients, moves people between those roles, reads the audit. |
| developer | Uploads apps for approval. |
| reviewer | Approves uploads. |
| recipient | Opens the organisation's apps, and nothing else. |

Two rules hold whatever your role:

- **Nobody approves their own upload.** A second person always looks.
- **An organisation always has an owner.** Removing or demoting the last
  owner is refused, even when two people try at the same moment.

## Setting one up

Create it. You become its first owner:

```sh
krate org create acme --name "Acme"
```

Register the organisation's root key. This is the publisher root key that
`krate delegate` uses; only its public half leaves your machine:

```sh
krate org root-key acme acme-root.pkcs8
```

Invite people, one person and one role at a time, by GitHub username or by
work email:

```sh
krate org invite acme bob --role reviewer
krate org invite acme carol --role developer
krate org invite acme --email dan@acme.example --role recipient
```

Each invitation prints the one command that person runs, and a link that
does the same in a browser. It works once, only for the person it names,
for 7 days:

```sh
krate org join acme <code>
```

or open `https://krate.tech/join/?o=acme&c=<code>`.

`krate org list` shows the organisations you are in and your role in each.
`krate org show acme` lists who is in it, each as `bob (GitHub)` or
`dan@acme.example (email)`. `krate org role acme bob developer` changes a role
(name an email member by their address), and `krate org cancel-invite acme <code>` takes back an
invitation nobody has used. Making someone an owner or an admin, or changing
an owner's or admin's role, is for an owner.

## Sending an app

Apps sent through an organisation are signed, so everyone can tell the app
came from the organisation and was not changed on the way.

The shortest way is a release key made on the developer's own machine:

```sh
krate org release-key acme
```

It prints the key's public half and the exact line an owner runs to
delegate it. The private key never leaves the machine. The owner sends back
the file that line writes, and the developer installs it:

```sh
krate org release-key acme --delegation acme-release.json
```

From then on `krate publish app.krate --org acme` marks and signs the app
itself, and
Krate Studio's Send menu offers "Send to Acme for approval", which does the
same and then shows "Waiting for approval in Acme" with a link to the
Waiting tab.

Or by hand: the owner delegates a release key to the developer once:

```sh
krate delegate --root acme-root.pkcs8 --key carol-release.pkcs8 \
  --namespace 'acme/*' -o carol-acme.json
```

The developer marks the app as Acme's private app and signs it, in one
command, and uploads it:

```sh
krate sign app.krate --org acme --key carol-release.pkcs8 \
  --delegation carol-acme.json
krate publish app.krate --org acme
```

The mark is a small file inside the app, `ext/krate/org/private.json`, and
the signature covers it, so it cannot be removed or changed without breaking
the signature. Only a marked app is an organisation's private app: an app
without the mark, signed or not, never contacts the hub when it opens, and a
marked app cannot be published publicly.

An unsigned or unmarked app is turned back before anything is uploaded, with
the exact `krate sign --org` line to fix it. The hub also refuses an app signed under a delegation
that had already expired, and anything signed by a release key the root has
withdrawn. A signed one becomes a candidate: it waits for
approval and opens for nobody yet. The upload prints its digest, the
fingerprint of its exact bytes. Big apps take the same path as any other
publish (see [Publishing and opening big apps](publishing-big-apps.md)).

A reviewer approves that exact digest:

```sh
krate org approve acme <digest>
```

The approval is for those bytes only. If the app changes after it was
reviewed, the new upload has a different digest and waits for its own
approval. Approving the old digest does not release the new bytes.

Once it is approved, the reviewer is given the link. Members and recipients
open it with `krate run`:

```sh
krate run https://hub.krate.tech/a/<digest>
```

`krate run` sends the person's sign-in with the link, and the hub checks it
against the organisation every time the app is fetched. Krate sends the
sign-in only to a Krate hub, never to whatever site a link points at.

`krate org releases acme` lists every release, with who uploaded it and who
approved it. `krate org candidates acme` lists every upload, waiting or
released, for the people who publish, approve or run the organisation.

## Opening it offline

The first time a member opens a private app, Krate shows the terms with the
exact date: "Works offline until <date>, then needs a connection. If you are
removed, it stops when it next connects." Each open while online checks in
with the hub and renews that offline access when it is half used. Owners
choose how long an app works offline: 7 days unless they say otherwise, 30
at most, or not at all, in which case it opens only with a connection.

Offline, the app opens only on that stored access, for that app and that
sign-in, until it runs out. When the device connects again and the check-in
is refused, because the person was removed, the access is deleted and the
app does not open. The person's own data in it is kept.

This works for every way an app is opened: `krate run`, `krate launch`, a
double-click, Krate Studio's Open, and an AI tool through `krate mcp`. Each
says in words why a private app did not open, and how to sign in; on a
double-click on a Mac or Windows, in a window. A marked app whose signature
does not hold is refused too.

The limit to know: this is enforced by Krate on the device. Someone who
strips the mark and the signature has an unsigned copy of the code, not the
organisation's app, and Krate cannot stop a file from being copied.

## Your company's apps on every computer

Every member sees the apps their organisations released to them, on any
computer with Krate:

```sh
krate org apps
```

It lists each app you may open now, with its version, its size and what it
can do on your computer, in the words the reviewer saw when they approved
it. Only apps for you are listed: an app limited to groups you are not in is
not there, and a withdrawn release is gone. Open one by its app id:

```sh
krate org apps acme --open com.acme.expenses
```

That opens the release the organisation chose to open now, through the
same checks as any private app: after a rollback it is the older one.

Apps that may be used offline are kept ready on your computer when they are
listed, with their offline access, so they open at once, and with no
connection until that access runs out. With no connection, `krate org apps`
shows the list it last saw. Krate Studio shows the same apps on its Home
under Company apps, refreshed when it opens and every 15 minutes while it is
open. `krate org apps --json` is the same list for other tools.

## Company connections

A connection lets the organisation's apps call one of its APIs without any
app holding the API's credential. An owner or admin sets it up once, in
Settings on the dashboard or with the CLI:

```sh
krate org connection create acme finance-api \
  --base https://api.acme.com/finance \
  --header Authorization --value-file finance-token.txt \
  --allow GET --paths /invoices,/rates \
  --groups finance --apps com.acme.ledger
```

The credential is read from the file, or typed at a prompt that does not
show it, and is never shown again: the dashboard and `krate org connection
list acme` say only which header it is, who set it and when, and how often
it was used. `krate org connection replace` gives it a new credential, which
every app gets the next time it opens; `krate org connection remove` deletes
it. Moving a connection to another address needs its credential again.

An app asks for it by name in its manifest:

```toml
[[capabilities]]
cap = "net.connection:finance-api"
rationale = "Read invoices"
```

and makes ordinary requests to `https://api.acme.com/finance/...`, with no
credential of its own. When the organisation's signed app opens, Krate asks
the hub for the credential for this person and this release, and adds it to
the app's requests to that address, after they leave the app. It is given
only to a member in the connection's groups, for an app it allows, in a
version that asked for it, and not for a withdrawn release. A request with a
method or path the connection does not allow, or offline, is refused in
words and never sent. A redirect is never followed with it, and no request
anywhere else carries it. Each use is counted per day in the audit.

The request goes from the person's computer, so an API on the company
network works. The limit to know: a server that echoes request headers back
would show the credential to the app.

## Withdrawing a release key

If a release key is lost or someone leaves, the owner withdraws it with
`krate revoke` and gives the hub the list it writes, with the root key:

```sh
krate revoke --root acme-root.pkcs8 --key carol-release.pkcs8 \
  --reason "laptop lost" -o acme-revocations.json
krate org root-key acme acme-root.pkcs8 --revocations acme-revocations.json
```

From then on the hub refuses new uploads signed by that key, and an upload
it signed that is still waiting cannot be approved. It has to be signed
again with a current key and uploaded again.

## Removing someone

```sh
krate org remove acme dan@acme.example
```

Dan's access ends at once: the next time he opens the app, he is told he
was removed and nothing runs. His own apps and his account are untouched.
The organisation's releases stay, and so does the record of who uploaded
and who approved each one.

Anyone can leave an organisation with `krate org remove acme <their own
login>`.

## The audit

```sh
krate org audit acme
```

Owners and admins see every change and every refused attempt: who did it,
when, and to what. Each event is chained to the one before it, and the hub
checks the whole chain each time. If any event was changed after it was
written, the command says so and fails. The audit never holds app files,
source, keys or tokens.

## When something is refused

A refusal says why and what to do next, for example:

- "you can't approve your own upload: ask a reviewer in acme"
- "you were removed from acme, so this app no longer opens for you"
- "that invitation is for a different GitHub account, not erin"
- "that would leave acme with no owner, so nothing changed"

## Not there yet

The pilot is the smallest organisation path that works. These are not in
it:

- **Single sign-on.** No SAML, OIDC or SCIM. People sign in with GitHub.
- **Billing in the product.** The pilot is invoiced by hand.
- Moving an organisation's apps to another organisation, choosing where
  its data is stored, exporting it, and closing an organisation.
