---
title: Kepta — Privacy Policy
description: Kepta collects nothing. There is no account, no server, and no network permission.
sidebar: false
outline: false
prev: false
next: false
---

# Privacy Policy for Kepta

**Last updated: 4 October 2026**

Kepta is a personal memory app for Android, published by ThinkGrid Labs. It is
built on [TalaDB](/introduction), an embedded database that runs entirely on the
device — this page is hosted alongside TalaDB's documentation because both are
published by the same developer.

## The short version

**Kepta collects nothing.** There is no account, no server, no analytics, no
advertising, and no automatic crash reporting. Everything you write stays on
your phone. If something goes wrong, you can choose to email us a problem
report — see [Problem reports](#problem-reports).

This is not a promise about how we choose to behave. The released app is built
**without Android's internet permission**, so the operating system will not let
it open a network connection at all. You can verify this yourself: turn on
airplane mode and keep using the app — nothing changes, because nothing in it
ever needed a connection.

## What Kepta stores, and where

Everything you enter — the things you record, the memories you write, any photos
you attach — is stored in Kepta's private storage on your device:

- Your records are kept in an encrypted database.
- Photos and other attachments are encrypted before they are written to storage,
  and decrypted only while you are viewing them.
- The encryption key is protected by the PIN you choose when you first open the
  app. Your PIN itself is never stored anywhere.

None of it is transmitted to us or to anyone else, because there is nothing in
the app capable of transmitting it.

## What we receive

Nothing, unless you send us a problem report. We have no servers that Kepta
communicates with, so we do not receive, store, process, or have any means of
accessing your information. We cannot read your records, and we cannot recover
them for you if you lose access to them.

## Problem reports

Kepta keeps a short record of errors on your phone: what failed, where in the
app's code, and which version of Kepta on which model of phone and version of
Android. It never contains your things, memories, names, photos or anything
else you wrote — the code that writes it does not read your records. It is
deleted when you uninstall the app.

If Kepta closes unexpectedly, or you choose **More → Report a problem**, the app
offers to send that record to us. It opens your email app, or another app you
pick, with the report already filled in and addressed to us. You can read
exactly what it contains, change it or delete it before sending, and nothing is
sent if you do not send it. Kepta does not send it itself — it has no internet
permission.

If you do send one, we receive the report and your email address, and use them
only to find and fix the problem and to reply to you.

## Permissions the app requests

| Permission | Why | What leaves your device |
|---|---|---|
| **Camera** | To photograph a receipt, a serial number or an item, and attach it to a memory. Only used when you choose to take a photo. | Nothing. The photo is encrypted and saved locally. |
| **Notifications** | To show reminders you have asked for — for example, a warranty running out. | Nothing. Reminders are scheduled by your phone; there is no push service. |
| **Run at startup** | So reminders you have already scheduled survive a restart. | Nothing. |
| **Biometrics / fingerprint** | Optional, as a faster way to unlock instead of typing your PIN. | Nothing. Biometric data never leaves the device's secure hardware and is never seen by the app. |

Kepta requests **no internet permission**, no location, no contacts, and no
access to your general files or photo library. Choosing an existing photo uses
Android's own picker, which hands Kepta a single image you selected without
giving it access to anything else.

## Sharing and disclosure

We do not share your information, because we do not have it. We cannot sell it,
disclose it to third parties, or hand it over in response to a legal request —
there is nothing on our side to hand over.

If you choose to export your records, Kepta creates a plain file and passes it to
whatever app you select — email, a cloud drive, a messaging app. From that point
the file is handled by that app and by whoever you send it to, under their terms
rather than ours.

## Deleting your data

Uninstalling Kepta removes everything it stored. You can also clear it at any
time from Android's app settings ("Clear storage"). Because nothing is backed up
to us or to any cloud service, deletion is immediate and complete, and there is
no copy anywhere for us to delete on your behalf.

## Children

Kepta is a general-audience app and is not directed at children. It collects no
personal information from anyone, including children.

## Changes to this policy

If a future version of Kepta ever adds a feature that transmits data — for
example an optional, encrypted backup across your own devices — this policy will
be updated before that version is released, and the app itself will say clearly
what has changed. Any such feature would require the app to request internet
permission, which is publicly visible on its Play Store listing.

## Contact

Questions about this policy: **dtpaler@gmail.com**
