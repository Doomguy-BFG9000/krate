# Organisations

An organisation is a group of people on a Krate hub who send apps to each
other privately. One person uploads an app, a second person approves it,
and then only the organisation's members and the people it sends to can
open it. A copied link alone opens nothing.

Organisations are a pilot, run with a few design partners while it is new.
Everything below is driven from the `krate` command line.

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
came from the organisation and was not changed on the way. The owner
delegates a release key to the developer once:

```sh
krate delegate --root acme-root.pkcs8 --key carol-release.pkcs8 \
  --namespace 'acme/*' -o carol-acme.json
```

The developer signs the app with it and uploads it:

```sh
krate sign app.krate --key carol-release.pkcs8 --namespace acme/app \
  --delegation carol-acme.json
krate publish app.krate --org acme
```

An unsigned app is turned back before anything is uploaded, with the exact
commands to sign it. The hub also refuses an app signed under a delegation
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
- **Offline access.** Opening a private app needs the hub at that moment;
  there are no offline leases.
- **Billing in the product.** The pilot is invoiced by hand.
- Moving an organisation's apps to another organisation, choosing where
  its data is stored, exporting it, and closing an organisation.
