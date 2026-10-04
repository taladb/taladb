---
title: Notewell — Privacy Policy
description: Notewell collects nothing. There is no account, no server, and no network permission.
sidebar: false
outline: false
prev: false
next: false
---

# Privacy Policy for Notewell

**Last updated: 4 October 2026**

Notewell is an observation notebook for teachers on Android, published by
ThinkGrid Labs. It is built on [TalaDB](/introduction), an embedded database
that runs entirely on the device — this page is hosted alongside TalaDB's
documentation because both are published by the same developer.

## The short version

**Notewell collects nothing.** There is no account, no server, no analytics, no
advertising, and no automatic crash reporting. Your classes, students and notes
stay on your phone. If something goes wrong, you can choose to email us a
problem report — see [Problem reports](#problem-reports).

This is not a promise about how we choose to behave. The released app is built
**without Android's internet permission**, so the operating system will not let
it open a network connection at all. You can verify this yourself: turn on
airplane mode and keep using the app — nothing changes, because nothing in it
ever needed a connection.

## What Notewell stores, and where

Everything you enter — class names, student names, your observations and the
tags on them — is stored in Notewell's private storage on your device:

- Your notebook is kept in an encrypted database.
- The encryption key is protected by the Android Keystore, the phone's own
  secure key storage.
- Notewell's settings are kept in the same encrypted database.

**Search by meaning** works the same way. The language model that compares
notes by meaning is part of the app and runs on your phone, so your notes are
never sent anywhere to be analysed.

Android's own cloud backup and device-to-device transfer are **turned off** for
Notewell, so none of this is copied into a Google backup or onto another phone.

None of it is transmitted to us or to anyone else, because there is nothing in
the app capable of transmitting it.

## What we receive

Nothing, unless you send us a problem report. We have no servers that Notewell
communicates with, so we do not receive, store, process, or have any means of
accessing your information. We cannot read your notes, and we cannot recover
them for you if you lose access to them.

## Problem reports

Notewell keeps a short record of errors on your phone: what failed, where in the
app's code, and which version of Notewell on which model of phone and version of
Android. It never contains your notes, classes or students — the code that
writes it does not read your notebook. It is deleted when you uninstall the app.

If Notewell closes unexpectedly, or you choose **Settings → Report a problem**,
the app offers to send that record to us. It opens your email app, or another
app you pick, with the report already filled in and addressed to us. You can
read exactly what it contains, change it or delete it before sending, and
nothing is sent if you do not send it. Notewell does not send it itself — it has
no internet permission.

If you do send one, we receive the report and your email address, and use them
only to find and fix the problem and to reply to you.

## Permissions the app requests

| Permission | Why | What leaves your device |
|---|---|---|
| **Biometrics / fingerprint** | Optional. Only used if you turn on **Lock Notewell**, to ask for your fingerprint, face or phone PIN through Android's own prompt. | Nothing. Biometric data never leaves the device's secure hardware and is never seen by the app; Android only tells Notewell whether the check passed. |

Notewell requests **no internet permission**, no camera, no location, no
contacts, no microphone, and no access to your general files or photo library.

## Backups you make

If you choose **Back up now**, Notewell writes one file, encrypted with a
passphrase you choose, to a place you pick with Android's own file picker — for
example your phone's storage or a cloud drive app. Restoring works the same way
in reverse: Android hands Notewell only the single file you select.

Where a backup file goes, and who can access it there, is up to you and the
service you choose, under their terms rather than ours. Without your passphrase
the file cannot be read, and we cannot recover it for you.

## Sharing and disclosure

We do not share your information, because we do not have it. We cannot sell it,
disclose it to third parties, or hand it over in response to a legal request —
there is nothing on our side to hand over.

## Deleting your data

Delete a note, a student or a whole class from within the app. To remove
everything, uninstall Notewell or clear its storage from Android's app settings
("Clear storage"). Because nothing is backed up to us or to any cloud service,
deletion is immediate and complete, and there is no copy anywhere for us to
delete on your behalf. Backup files you saved yourself are deleted wherever you
saved them.

## Children and student information

Notewell is intended for teachers and is not directed at children. It collects
no personal information from anyone, including children.

The notes you write may be about your students. They stay on your phone and in
any backups you make; teachers remain responsible for following their school's
policies on recording and keeping student information.

## Changes to this policy

If a future version of Notewell ever adds a feature that transmits data, this
policy will be updated before that version is released, and the app itself will
say clearly what has changed. Any such feature would require the app to request
internet permission, which is publicly visible on its Play Store listing.

## Contact

Questions about this policy: **dtpaler@gmail.com**
