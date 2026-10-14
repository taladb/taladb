# Tripsko privacy policy

_Last updated: 14 October 2026_

Tripsko keeps a traveller's bookings, travel documents and notes on their
phone, ready offline. It is built so that they stay there.

## What Tripsko collects

**Nothing.** The developer does not receive, collect, store or share any
information from the app — no bookings, no documents, no usage data, no
automatic crash reports, no identifiers. The app has no account system, no
analytics and no advertising.

Tripsko does not request the Android internet permission, so it is not able
to send data off the device.

## What stays on your phone

Trips, bookings, notes and settings are stored only on your device, in a
database encrypted with a key held by the Android Keystore. Documents you add
— tickets, confirmations, screenshots — are stored as files encrypted with a
second Keystore-protected key, and are decrypted only in memory to show them.

Reading booking details out of a document (a PDF's text, or text recognition
on a photo or scan) happens entirely on your phone; the recognition models are
part of the app. Nothing is sent anywhere to be read.

Android's own cloud backup and device-to-device transfer are turned off for
Tripsko, so this data is not copied into a Google backup.

## Documents you share into Tripsko

When you share a file to Tripsko from another app, or pick one with the
system file picker, Tripsko reads that one file and keeps its own encrypted
copy. It cannot see your other files and asks for no storage permission.

## Exports you make

If you choose **Export all trips**, Tripsko writes one file, encrypted with a
passphrase you choose, to a location you pick with the system file picker —
for example your phone's storage or a cloud drive. Where that file goes, and
who can access it there, is up to you and the service you choose. Without the
passphrase the file cannot be read; the developer cannot recover it.

## Reminders and the widget

If you turn on reminders, Tripsko shows notifications before flights, trains,
activities and check-outs. They never contain booking codes, and a locked
phone shows only "Trip reminder". The home-screen widget shows the next
booking's name and time, never a booking code; with **Lock Tripsko** on, it
shows only what kind of booking is next and when.

## Problem reports you choose to send

If Tripsko closes unexpectedly, or hits an error it recovers from, it writes
a short technical record of it in its private storage. Nothing is sent
automatically. After a crash the app offers to send that record, and
**Settings → Report a problem** does the same at any time: Tripsko hands the
report to an app you pick, usually email, and you see its full text before
sending. A report holds your phone's model and Android version, Tripsko's
version and the technical details of the error — no bookings, documents or
names. The developer uses a report only to fix the problem and deletes it on
request.

## Permissions

- **Notifications** — only if you turn on reminders.
- **Run at startup** — to set the next reminder again after the phone
  restarts.
- **Use biometrics / fingerprint** — only if you turn on **Lock Tripsko**,
  to ask for your fingerprint, face or phone PIN through the system prompt.
  Tripsko never sees your biometric data.

## Deleting your data

Delete a booking, document or trip in the app, or everything by uninstalling
Tripsko or clearing its storage in Android settings. Export files you saved
are deleted wherever you saved them.

## Children

Tripsko is intended for adults. It is not directed at children and does not
collect information from anyone.

## Changes and contact

Changes to this policy will be posted at the same address with a new date.
Questions: dtpaler@gmail.com.

The published copy is https://taladb.dev/tripsko/privacy-policy.html
(source: `taladb/docs/tripsko/privacy-policy.md`); keep the two in step.
