# Data URIs

An image can carry its own bytes. Both URIs below are the PNGs from
`assets/`, base64 encoded, so every spelling of the same content resolves to the
same asset id — nothing is read from disk and nothing is fetched.

![the red pixel, inline](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mO4o6HxHwAFPAIs0Zo91QAAAABJRU5ErkJggg==)

![the checkerboard, inline](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAHElEQVR42mOQk5P7/+HDh/+4aAZ8kiCaYViYAADs9KUBExt2ZgAAAABJRU5ErkJggg==)

Repeating the first URI must not create a second asset:

![the red pixel again](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mO4o6HxHwAFPAIs0Zo91QAAAABJRU5ErkJggg==)

A data URI is a URI like any other, so it can also be linked instead of shown:

[download the pixel](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mO4o6HxHwAFPAIs0Zo91QAAAABJRU5ErkJggg==)

`mdx assets` lists the manifest this produces: two assets, the red pixel
referenced three times.
