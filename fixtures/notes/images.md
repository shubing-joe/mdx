# Images

Local images resolve against the document's own directory
(`fixtures/notes/`), and their bytes never leave the machine.

![A single red pixel](assets/pixel.png)

The same syntax carries a title, and an empty alt text is allowed:

![An 8 by 8 checkerboard](assets/checker.png "A checkerboard")
![](assets/pixel.png)

## Reference style, lists and links

![reference image][pixel]

- a list item holding an image:
  ![inside a list](assets/pixel.png)
- [![a linked image](assets/checker.png)](https://typst.app)

[pixel]: assets/pixel.png "Definition for the reference style image"

## Repeated references

The store is content addressed, so the three images below collapse into one
asset:

![one](assets/pixel.png)
![two](assets/pixel.png)
![three](assets/pixel.png)
