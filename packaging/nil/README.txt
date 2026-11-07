nil 0.1.1
=========

nil is a bit-perfect music player for Windows. It sends your files to your DAC
exactly as they are: no resampling, no volume change, no mixing with other
sounds. The name is the promise: nothing added.


GETTING STARTED
---------------

1. Double-click nil.exe. There is nothing to install.
2. On the first run, click "Add music folder" and pick the folder that holds
   your music. nil reads the folder (about half a minute for ~2,700 files) and
   shows your albums. Later launches open instantly and look for changes in
   the background. While nil is open, music you add to or remove from the
   folder shows up a few seconds after the copy finishes.
3. Open an album and double-click a track to play it.

To keep nil handy, right-click nil.exe (or nil on the taskbar while it runs)
and choose "Pin to taskbar" or "Pin to Start".


FIND YOUR WAY AROUND
--------------------

Home        Your most recently added albums (newest files first).
Albums      Every album, sorted by artist.
Artists     Every artist; click one to see their albums.
Search      Type in the search box to filter artists, albums and tracks.
Playlists   Your saved playlists.
Settings    The gear: music folders, rescanning, files nil couldn't read,
            your DAC's capabilities, and plugins.

The bar at the bottom shows what's playing. Click its cover for the
full-window Now Playing view with the queue. The button at the bottom right
picks the output device (DAC).

The badge next to the format tells you what reaches the DAC:
  Bit-perfect      the file's samples arrive unchanged.
  Not bit-perfect  the file had to be decoded to a different form first
                   (AAC files, which are lossy).


KEYBOARD AND MOUSE
------------------

Space               Play / pause
Left / Right        Seek back / forward 10 seconds
Ctrl+Left           Previous track (restarts the track after 3 seconds)
Ctrl+Right          Next track
Ctrl+F              Search
Esc or Backspace    Go back (leave an album, close Now Playing)
Alt+Left / Right    Back / forward, like a browser
Mouse back/forward  Back / forward

The window: drag the empty part of the top bar to move it, double-click it
to maximize, and drag any edge to resize. Only one nil runs at a time:
starting it again brings the open window to the front.


WHAT IT PLAYS
-------------

FLAC (MQA-encoded FLAC passes through untouched), WAV, AIFF, ALAC (.m4a),
AAC-LC (.m4a; lossy, so it is shown as not bit-perfect), DSF and DFF (DSD).

DSD is sent as DoP (DSD over PCM), up to the highest rate your DAC accepts.
DoP is off for each DAC until you turn it on in Settings > Audio: Windows
can't tell nil whether a DAC decodes DoP, and one that doesn't plays it as
hiss. Windows sometimes sees a DAC as a new device (after a driver change,
or in another USB port); turn DoP on again there.
A track your DAC can't play as it is (for example DSD256 on a DAC that stops
at DSD128) is skipped with a short note in the bar; nil never converts it
behind your back.

Output uses WASAPI exclusive mode: while nil plays, it has the DAC to itself
and other programs' sound won't play through that device.


REQUIREMENTS
------------

Windows 10 or 11, 64-bit. A USB DAC (or any output that allows exclusive
mode). In Windows Sound settings > your device > Properties > Advanced, keep
"Allow applications to take exclusive control of this device" turned on.


PLAYLISTS
---------

The Playlists tab lists your saved playlists. Open one to Play it, add it to
the queue (Queue), Rename it or Delete it. To save what is playing, open Now
Playing and choose "Save queue as playlist". Playlists are ordinary .m3u8
files, so other players can read them, and nil can open playlists written by
other players. A track whose file has gone is shown greyed out and skipped.


PLUGINS
-------

A plugin is a separate program that adds a feature to nil, for example a
playlist maker. To add one, put its folder inside the plugins folder next to
nil.exe, then open Settings > Plugins and switch it on. A new plugin is always
off until you do. nil shows what it asks for first: reading your library,
controlling playback, or saving playlists. A plugin is an ordinary program
running as you, so those permissions limit what it can do through nil, not
what it can do on your PC. Only switch on plugins you trust.

A running plugin may add its own tab to the top bar. If a plugin crashes, nil
restarts it a couple of times and then shows "Crashed" in Settings > Plugins,
with the last lines it printed and a Restart button.

Plugin authors: see PLUGIN-API.md at https://github.com/reubinars/nil


WHERE NIL KEEPS ITS DATA
------------------------

Everything lives in the folder that holds nil.exe, so you can copy or move
that folder as a whole and take everything with you:

data\                     settings, music folder list, library index, device
                          profiles, plugin choices, album cover thumbnails
data\playlists\           your playlists (.m3u8 files)
data\logs\bp.log          nil's log
data\logs\plugins\       what each plugin printed (one .log per plugin)
plugins\                  one folder per plugin; its own files are kept in
                          plugins\<name>\data

Updating nil replaces only nil.exe and the documents, never data\ or
plugins\. The first time you start this version, it copies your settings,
library index and thumbnails from the old locations (%APPDATA%\bitperfect and
%LOCALAPPDATA%\bitperfect) into data\, then removes the old folders. If the
folder holding nil.exe can't be written to (for example C:\Program Files),
nil keeps its data in %APPDATA%\nil instead, and Settings says so.

Settings shows where the data is and has an "Open folder" button. To reset
nil completely, close it and delete the data folder. Your music files are
never changed.


TROUBLESHOOTING
---------------

No sound, or "device in use"
    Another program has the DAC in exclusive mode, or exclusive mode is
    turned off for the device (see REQUIREMENTS). Close the other program and
    press Retry in the bar.

A track is skipped
    The bar says why: usually the DAC doesn't accept that sample rate or DSD
    rate. Settings > Audio shows exactly what your DAC accepts. For DSD
    tracks, check that DSD over PCM (DoP) is on there.

The DAC was unplugged
    nil stops and shows an error with Retry. Plug it back in and press Retry,
    or pick another device from the device button.

Some files are missing from the library
    Settings lists the files nil couldn't read and why.

Anything else
    Look at the end of data\logs\bp.log, next to nil.exe.


LICENCES
--------

The fonts and libraries built into nil.exe are listed with their licences in
the licenses folder.
