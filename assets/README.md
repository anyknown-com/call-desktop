`icon.png` is the 1024×1024 source. `AppIcon.icns` is derived from it (artwork scaled to 824 px and centred on a transparent 1024 canvas, per macOS icon proportions), regenerate with:

```sh
cd assets && python3 -c "
from PIL import Image
src=Image.open('icon.png').convert('RGBA').resize((824,824),Image.LANCZOS)
c=Image.new('RGBA',(1024,1024),(0,0,0,0)); c.paste(src,(100,100),src)
import os; os.makedirs('AppIcon.iconset',exist_ok=True)
for s in [16,32,128,256,512]:
    c.resize((s,s),Image.LANCZOS).save(f'AppIcon.iconset/icon_{s}x{s}.png')
    c.resize((s*2,s*2),Image.LANCZOS).save(f'AppIcon.iconset/icon_{s}x{s}@2x.png')
" && iconutil -c icns AppIcon.iconset -o AppIcon.icns && rm -rf AppIcon.iconset
```
