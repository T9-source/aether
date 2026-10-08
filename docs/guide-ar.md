# دليل Aether السريع (بالعربي)

> **Aether** — بروكسي أمان ويب محلي، مفتوح المصدر، للمطورين.
> المؤسس: **smart-jemy** · الرخصة: Apache-2.0 · بدون أي تيليمتري.

## البداية السريعة

```bash
# شغّل الـ daemon (الذاكرة فقط للمحاولة الأولى)
cargo run -p aether-cli -- serve --memory

# صدّر شهادة الـ CA وثبّتها في المتصفح مرة واحدة
cargo run -p aether-cli -- ca export --out aether-ca.pem
```

ثم وجّه المتصفح على البروكسي `127.0.0.1:8888` — أي موقع HTTPS هتشتغل
**بدون أي تحذير شهادات**، وكل طلب/استجابة هتتسجل.

## أوامر يومية

```bash
aether flows list                 # آخر الطلبات المسجلة
aether flows show 1               # تفاصيل flow كامل (headers + body)
aether flows search "login"       # بحث نصي (FTS5)

aether intercept set --request on # إمساك الطلبات الجديدة
aether intercept ls               # الطلبات الممسوكة
aether intercept show 7           # اعرض الطلب JSON
aether intercept release 7 --header "X-Reviewed: yes"
aether intercept drop 7           # أو ارفضه (502 للعميل)

aether rules add --file rule.json # قاعدة match & replace حية
aether bench flows --n 100000     # اختبار الحجم (Milestone-0)
```

## مثال قاعدة جاهزة

```json
{
  "name": "stamp-review",
  "stage": "request",
  "host_include": ["*.example.com"],
  "action": { "kind": "set_request_header", "name": "X-Reviewed", "value": "yes" }
}
```

## خريطة الكود باختصار

- `crates/aether-core` — الموديل والـ pipeline والقواعد (معزول تمامًا عن الشبكة)
- `crates/aether-http` — محرك الشبكة: h1 مكتوب يدويًا + MITM بالـ rustls
- `crates/aether-store-sqlite` — التاريخ في SQLite مع بحث FTS5
- `crates/aether-daemon` — الـ daemon الوحيد صاحب الحالة + الـ control plane
- `apps/aether-cli` — عميل نحيف للـ daemon (بدون UI نهائيًا — إثبات ADR-001)

## الخطوة القادمة (V0.2)

فحص HTTP/2 وWebSocket، إعادة استخدام اتصالات الـ upstream، spooling للـ bodies
الكبيرة — والموديل جاهز ليها من اليوم الأول (ADR-003).
