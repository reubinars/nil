nil 0.1.0
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
   the background.
3. Open an album and double-click a track to play it.

To keep nil handy, right-click nil.exe (or nil on the taskbar while it runs)
and choose "Pin to taskbar" or "Pin to Start".


FIND YOUR WAY AROUND
--------------------

Home        Your most recently added albums (newest files first).
Albums      Every album, sorted by artist.
Artists     Every artist; click one to see their albums.
Search      Type in the search box to filter artists, albums and tracks.
Settings    The gear: music folders, rescanning, files nil couldn't read,
            and your DAC's capabilities.

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
to maximize, and drag any edge to resize.


WHAT IT PLAYS
-------------

FLAC (MQA-encoded FLAC passes through untouched), WAV, AIFF, ALAC (.m4a),
AAC-LC (.m4a; lossy, so it is shown as not bit-perfect), DSF and DFF (DSD).

DSD is sent as DoP (DSD over PCM), up to the highest rate your DAC accepts.
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


WHERE NIL KEEPS ITS DATA
------------------------

nil.exe can live anywhere; moving it loses nothing.

%APPDATA%\bitperfect\              settings, music folder list, library index,
                                   device profiles, and the log (bp.log)
%LOCALAPPDATA%\bitperfect\thumbs\  album cover thumbnails

(Paste those paths into the Explorer address bar to open them.) To reset nil
completely, close it and delete both folders. Your music files are never
changed.


TROUBLESHOOTING
---------------

No sound, or "device in use"
    Another program has the DAC in exclusive mode, or exclusive mode is
    turned off for the device (see REQUIREMENTS). Close the other program and
    press Retry in the bar.

A track is skipped
    The bar says why: usually the DAC doesn't accept that sample rate or DSD
    rate. Settings > Audio shows exactly what your DAC accepts.

The DAC was unplugged
    nil stops and shows an error with Retry. Plug it back in and press Retry,
    or pick another device from the device button.

Some files are missing from the library
    Settings lists the files nil couldn't read and why.

Anything else
    Look at the end of %APPDATA%\bitperfect\bp.log.


LICENCES
--------

The fonts and libraries built into nil.exe are listed with their licences in
the licenses folder.
