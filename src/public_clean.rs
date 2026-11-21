//! **Nettoyage à la sortie** des photos du site public (`public_site`) : ce qui part vers le public ne dit ni **où** la photo a
//! été prise (GPS) ni **avec quel appareil** (numéro de série, propriétaire, auteur). L'original n'est jamais touché : il reste
//! chiffré et complet dans l'uCloud du membre ; seule la copie servie est nettoyée.
//!
//! Formats reconnus **à leurs octets** (jamais au nom ni au type déclaré) :
//! - **JPEG** : segments APP1 (EXIF, XMP), APP2 hors profil de couleur (FlashPix, MPF), APP3 à APP15 (IPTC, Photoshop…) et
//!   commentaires retirés ; tout ce qui suit la fin d'image (seconde image MPF, traînées de fabricant) est coupé ;
//! - **PNG** : `eXIf`, `tEXt`, `zTXt`, `iTXt`, `tIME` et tout fragment annexe inconnu retirés, rien après `IEND` ;
//! - **WebP** : fragments `EXIF` et `XMP ` retirés (et leurs drapeaux `VP8X`), fragments inconnus retirés.
//!
//! L'**orientation** est gardée : un EXIF minimal (une seule étiquette, Orientation) remplace l'original quand la photo est
//! tournée. Les formats qui portent l'EXIF dans une structure que ce module ne réécrit pas (HEIC/HEIF, AVIF, TIFF) sont
//! **refusés** par le site public plutôt que servis bruts ([`Kind::Refused`]). Toute anomalie de structure (segment qui déborde,
//! fichier tronqué, fin d'image absente) est une erreur : on refuse, on ne sert jamais l'original.

/// Ce que disent les premiers octets d'un fichier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// JPEG, PNG ou WebP : nettoyé avant d'être servi.
    Clean,
    /// Image dont les métadonnées ne sont pas réécrites ici (HEIC/HEIF, AVIF, TIFF) : jamais servie publiquement.
    Refused,
    /// Autre chose (page, texte, vidéo…) : servi tel quel.
    Other,
}

/// Octets à lire en tête pour [`sniff`].
pub const SNIFF_LEN: u64 = 16;

/// Nature d'un fichier d'après ses premiers octets (au moins [`SNIFF_LEN`] quand le fichier est assez long).
#[must_use]
pub fn sniff(head: &[u8]) -> Kind {
    let at = |i: usize, s: &[u8]| head.get(i..i + s.len()) == Some(s);
    if at(0, &[0xFF, 0xD8, 0xFF]) || at(0, b"\x89PNG\r\n\x1a\n") || (at(0, b"RIFF") && at(8, b"WEBP")) {
        return Kind::Clean;
    }
    if at(0, b"II*\0") || at(0, b"MM\0*") || at(4, b"ftyp") && [&b"heic"[..], b"heix", b"hevc", b"heim", b"heis", b"mif1", b"msf1", b"avif", b"avis"].iter().any(|b| at(8, b)) {
        return Kind::Refused;
    }
    Kind::Other
}

/// Copie nettoyée de `data` (JPEG, PNG ou WebP). Toute structure douteuse est une erreur.
pub fn clean(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.starts_with(&[0xFF, 0xD8]) {
        jpeg(data)
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(data)
    } else if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        webp(data)
    } else {
        Err("format d'image non reconnu")
    }
}

/// EXIF minimal (TIFF petit-boutiste, une seule étiquette Orientation), ou rien si la photo n'est pas tournée.
fn orientation_only(tiff: &[u8]) -> Option<Vec<u8>> {
    let o = image::metadata::Orientation::from_exif_chunk(tiff)?.to_exif();
    (o != 1).then(|| {
        let mut t = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        t.extend_from_slice(&[o, 0, 0, 0, 0, 0, 0, 0]);
        t
    })
}

fn be16(d: &[u8], i: usize) -> Result<usize, &'static str> {
    d.get(i..i + 2).map(|b| usize::from(u16::from_be_bytes([b[0], b[1]]))).ok_or("fichier tronqué")
}

fn jpeg(d: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::with_capacity(d.len());
    out.extend_from_slice(&[0xFF, 0xD8]);
    let mut i = 2;
    let mut exif_done = false;
    loop {
        if d.get(i) != Some(&0xFF) {
            return Err("JPEG : marqueur attendu");
        }
        while d.get(i + 1) == Some(&0xFF) {
            i += 1; // octets de remplissage
        }
        let m = *d.get(i + 1).ok_or("fichier tronqué")?;
        i += 2;
        match m {
            0xD9 => {
                out.extend_from_slice(&[0xFF, 0xD9]);
                return Ok(out); // ce qui suit la fin d'image (seconde image, traînées) n'est jamais servi
            }
            0x01 | 0xD0..=0xD7 => out.extend_from_slice(&[0xFF, m]),
            0x00 | 0xD8 => return Err("JPEG : marqueur inattendu"),
            _ => {
                let len = be16(d, i)?;
                let end = i.checked_add(len).filter(|&e| len >= 2 && e <= d.len()).ok_or("JPEG : segment hors du fichier")?;
                let body = &d[i + 2..end];
                match m {
                    0xE1 => {
                        if let Some(tiff) = body.strip_prefix(b"Exif\0\0") {
                            if !exif_done {
                                exif_done = true;
                                if let Some(t) = orientation_only(tiff) {
                                    out.extend_from_slice(&[0xFF, 0xE1]);
                                    out.extend_from_slice(&u16::try_from(t.len() + 8).map_err(|_| "EXIF")?.to_be_bytes());
                                    out.extend_from_slice(b"Exif\0\0");
                                    out.extend_from_slice(&t);
                                }
                            }
                        }
                    }
                    0xE2 if body.starts_with(b"ICC_PROFILE\0") => out.extend_from_slice(&d[i - 2..end]),
                    0xE0 if body.starts_with(b"JFIF\0") || body.starts_with(b"JFXX\0") => out.extend_from_slice(&d[i - 2..end]),
                    0xE0..=0xEF | 0xFE => {} // APPn, commentaire : métadonnées
                    _ => out.extend_from_slice(&d[i - 2..end]),
                }
                i = end;
                if m == 0xDA {
                    // Données d'image : jusqu'au prochain vrai marqueur (FF suivi d'autre chose que 00, RSTn ou FF).
                    let mut j = i;
                    loop {
                        match (d.get(j), d.get(j + 1)) {
                            (Some(0xFF), Some(&n)) if n != 0x00 && n != 0xFF && !(0xD0..=0xD7).contains(&n) => break,
                            (Some(_), Some(_)) => j += 1,
                            _ => return Err("JPEG : fin d'image absente (fichier tronqué)"),
                        }
                    }
                    out.extend_from_slice(&d[i..j]);
                    i = j;
                }
            }
        }
    }
}

/// Fragments PNG annexes gardés (affichage seulement) ; tout autre fragment annexe est retiré.
const PNG_KEEP: [&[u8; 4]; 15] = [b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT", b"bKGD", b"pHYs", b"sPLT", b"hIST", b"acTL", b"fcTL", b"fdAT", b"cICP", b"cLLi"];

fn png_chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) -> Result<(), &'static str> {
    out.extend_from_slice(&u32::try_from(body.len()).map_err(|_| "PNG")?.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut h = crc32fast::Hasher::new();
    h.update(kind);
    h.update(body);
    out.extend_from_slice(&h.finalize().to_be_bytes());
    Ok(())
}

fn png(d: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut out = d[..8].to_vec();
    let mut i = 8;
    loop {
        let head = d.get(i..i + 8).ok_or("PNG : fichier tronqué")?;
        let len = usize::try_from(u32::from_be_bytes([head[0], head[1], head[2], head[3]])).map_err(|_| "PNG")?;
        let kind = &head[4..8];
        let end = i.checked_add(12).and_then(|x| x.checked_add(len)).filter(|&e| e <= d.len()).ok_or("PNG : fragment hors du fichier")?;
        let body = &d[i + 8..i + 8 + len];
        if !kind.iter().all(u8::is_ascii_alphabetic) {
            return Err("PNG : fragment invalide");
        }
        if kind == b"eXIf" {
            if let Some(t) = orientation_only(body) {
                png_chunk(&mut out, b"eXIf", &t)?;
            }
        } else if kind[0].is_ascii_uppercase() || PNG_KEEP.iter().any(|k| &k[..] == kind) {
            out.extend_from_slice(&d[i..end]);
        }
        i = end;
        if kind == b"IEND" {
            return Ok(out);
        }
    }
}

fn webp(d: &[u8]) -> Result<Vec<u8>, &'static str> {
    let riff = usize::try_from(u32::from_le_bytes([d[4], d[5], d[6], d[7]])).map_err(|_| "WebP")?;
    let total = riff.checked_add(8).filter(|&t| t <= d.len()).ok_or("WebP : fichier tronqué")?;
    let mut chunks: Vec<u8> = Vec::with_capacity(total);
    let (mut i, mut vp8x_at, mut has_exif) = (12, None, false);
    while i < total {
        let head = d.get(i..i + 8).filter(|_| i + 8 <= total).ok_or("WebP : fragment tronqué")?;
        let len = usize::try_from(u32::from_le_bytes([head[4], head[5], head[6], head[7]])).map_err(|_| "WebP")?;
        let end = i.checked_add(8).and_then(|x| x.checked_add(len)).filter(|&e| e <= total).ok_or("WebP : fragment hors du fichier")?;
        let padded = (end + (len & 1)).min(total);
        match &head[..4] {
            b"EXIF" => {
                let body = &d[i + 8..end];
                if let Some(t) = orientation_only(body.strip_prefix(b"Exif\0\0").unwrap_or(body)) {
                    chunks.extend_from_slice(b"EXIF");
                    chunks.extend_from_slice(&u32::try_from(t.len()).map_err(|_| "WebP")?.to_le_bytes());
                    chunks.extend_from_slice(&t); // longueur paire : pas de bourrage
                    has_exif = true;
                }
            }
            b"VP8X" => {
                vp8x_at = Some(chunks.len() + 8);
                chunks.extend_from_slice(&d[i..padded]);
            }
            b"VP8 " | b"VP8L" | b"ALPH" | b"ANIM" | b"ANMF" | b"ICCP" => chunks.extend_from_slice(&d[i..padded]),
            _ => {} // XMP et fragments inconnus
        }
        i = padded;
    }
    if let Some(f) = vp8x_at.and_then(|at| chunks.get_mut(at)) {
        *f = (*f & !0x0C) | if has_exif { 0x08 } else { 0 };
    }
    let mut out = Vec::with_capacity(chunks.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&u32::try_from(chunks.len() + 4).map_err(|_| "WebP")?.to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&chunks);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;

    /// EXIF hostile type : position GPS, numéro de série, propriétaire, orientation 6.
    fn exif_tiff() -> Vec<u8> {
        // IFD0 : Orientation=6, BodySerialNumber (dans IFD0 pour simplifier : seule la présence des octets compte), Artist, pointeur GPS.
        let mut t = b"II*\0\x08\0\0\0".to_vec();
        let serial = b"SN-SECRET-4242\0";
        let owner = b"Jean Proprietaire\0";
        let n: u16 = 4;
        let ifd_len = 2 + 12 * usize::from(n) + 4;
        let data_at = 8 + ifd_len;
        let gps_at = data_at + serial.len() + owner.len();
        t.extend_from_slice(&n.to_le_bytes());
        let entry = |tag: u16, ty: u16, cnt: u32, val: u32| [tag.to_le_bytes().as_slice(), &ty.to_le_bytes(), &cnt.to_le_bytes(), &val.to_le_bytes()].concat();
        t.extend(entry(0x0112, 3, 1, 6));
        t.extend(entry(0x013B, 2, owner.len() as u32, (data_at + serial.len()) as u32));
        t.extend(entry(0x8825, 4, 1, gps_at as u32));
        t.extend(entry(0xA431, 2, serial.len() as u32, data_at as u32));
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(serial);
        t.extend_from_slice(owner);
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend(entry(0x0001, 2, 2, u32::from_le_bytes(*b"N\0\0\0")));
        t.extend_from_slice(&0u32.to_le_bytes());
        t
    }

    fn photo_jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_fn(16, 8, |x, y| image::Rgb([(x * 15) as u8, (y * 30) as u8, 90]));
        let mut j = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut j, 90).encode_image(&img).unwrap();
        let mut seg = b"Exif\0\0".to_vec();
        seg.extend(exif_tiff());
        let xmp = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta>GPSLatitude 43.6 SN-SECRET-4242</x:xmpmeta>";
        let mut out = j[..2].to_vec();
        for (m, body) in [(0xE1u8, seg.as_slice()), (0xE1, xmp.as_slice()), (0xFE, b"Jean Proprietaire".as_slice())] {
            out.extend_from_slice(&[0xFF, m]);
            out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
            out.extend_from_slice(body);
        }
        out.extend_from_slice(&j[2..]);
        out.extend_from_slice(b"\xFF\xD8MPF-seconde-image SN-SECRET-4242");
        out
    }

    fn leaks(d: &[u8]) -> bool {
        let has = |n: &[u8]| d.windows(n.len()).any(|w| w == n);
        has(b"SN-SECRET") || has(b"Proprietaire") || has(b"GPSLatitude") || has(&0x8825u16.to_le_bytes()[..]) && has(b"N\0\0\0")
    }

    #[test]
    fn a_jpeg_loses_position_serial_and_owner_but_keeps_its_orientation() {
        let src = photo_jpeg();
        assert!(leaks(&src));
        assert_eq!(sniff(&src), Kind::Clean);
        let out = clean(&src).unwrap();
        assert!(!leaks(&out), "rien d'identifiant ne sort");
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (16, 8), "toujours décodable");
        let mut dec = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(&out)).unwrap();
        let exif = image::ImageDecoder::exif_metadata(&mut dec).unwrap().unwrap();
        assert_eq!(image::metadata::Orientation::from_exif_chunk(&exif).map(|o| o.to_exif()), Some(6), "orientation gardée");
        assert_eq!(clean(&out).unwrap(), out, "nettoyer deux fois ne change rien");
    }

    #[test]
    fn png_and_webp_lose_their_metadata_chunks() {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]));
        let mut p = Vec::new();
        image::codecs::png::PngEncoder::new(&mut p).write_image(img.as_raw(), 4, 4, image::ExtendedColorType::Rgba8).unwrap();
        let iend = p.len() - 12;
        let mut src = p[..iend].to_vec();
        png_chunk(&mut src, b"eXIf", &exif_tiff()).unwrap();
        png_chunk(&mut src, b"tEXt", b"Author\0Jean Proprietaire").unwrap();
        src.extend_from_slice(&p[iend..]);
        let out = clean(&src).unwrap();
        assert!(!leaks(&out) && leaks(&src));
        assert_eq!(image::load_from_memory(&out).unwrap().width(), 4);
        assert!(out.windows(4).any(|w| w == b"eXIf"), "orientation gardée dans un eXIf minimal");

        let mut w = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut w).encode(img.as_raw(), 4, 4, image::ExtendedColorType::Rgba8).unwrap();
        // WebP étendu : VP8X (drapeaux EXIF + XMP), image, EXIF, XMP.
        let mut chunks = b"VP8X\x0a\0\0\0\x0c\0\0\0\x03\0\0\x03\0\0".to_vec();
        chunks.extend_from_slice(&w[12..]);
        let tiff = exif_tiff();
        for (k, b) in [(b"EXIF", tiff.as_slice()), (b"XMP ", b"<x>GPSLatitude</x>".as_slice())] {
            chunks.extend_from_slice(k);
            chunks.extend_from_slice(&(b.len() as u32).to_le_bytes());
            chunks.extend_from_slice(b);
            if b.len() % 2 == 1 {
                chunks.push(0);
            }
        }
        let mut src = b"RIFF".to_vec();
        src.extend_from_slice(&((chunks.len() + 4) as u32).to_le_bytes());
        src.extend_from_slice(b"WEBP");
        src.extend(chunks);
        assert!(leaks(&src));
        let out = clean(&src).unwrap();
        assert!(!leaks(&out));
        assert_eq!(out[20] & 0x0C, 0x08, "drapeau EXIF seul (orientation), plus de XMP");
        assert_eq!(image::load_from_memory(&out).unwrap().width(), 4);
    }

    #[test]
    fn hostile_files_are_refused_never_served_raw_and_never_panic() {
        let src = photo_jpeg();
        // Fichier tronqué à chaque longueur : jamais de panique, et jamais l'original.
        for n in 0..src.len() {
            if let Ok(out) = clean(&src[..n]) {
                assert!(!leaks(&out), "tronqué à {n}");
            }
        }
        // Segment géant qui déborde, longueur < 2, EXIF malformé.
        let mut giant = vec![0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF];
        giant.extend_from_slice(b"Exif\0\0SN-SECRET");
        assert!(clean(&giant).is_err());
        assert!(clean(&[0xFF, 0xD8, 0xFF, 0xE1, 0, 1, 0xFF, 0xD9]).is_err());
        let mut bad = vec![0xFF, 0xD8, 0xFF, 0xE1, 0, 18];
        bad.extend_from_slice(b"Exif\0\0MM\0*\xff\xff\xff\xff\0\0");
        bad.extend_from_slice(&[0xFF, 0xD9]);
        assert_eq!(clean(&bad).unwrap(), [0xFF, 0xD8, 0xFF, 0xD9], "EXIF illisible : retiré, sans orientation");
        // PNG et WebP qui mentent sur leurs longueurs.
        assert!(clean(b"\x89PNG\r\n\x1a\n\xff\xff\xff\xf0IHDR").is_err());
        assert!(clean(b"RIFF\xff\xff\xff\xffWEBPVP8X").is_err());
        assert!(clean(b"RIFF\x10\0\0\0WEBPEXIF\xff\xff\xff\x7f").is_err());
        // Fuzz grossier : octets pseudo-aléatoires derrière chaque signature.
        let mut x: u32 = 2_463_534_242;
        for sig in [&b"\xFF\xD8\xFF"[..], b"\x89PNG\r\n\x1a\n", b"RIFF\x40\0\0\0WEBP"] {
            for _ in 0..300 {
                let mut d = sig.to_vec();
                for _ in 0..80 {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    d.push(x as u8);
                }
                let _ = clean(&d);
            }
        }
        for head in [&b"II*\0xxxxxxxxxxxx"[..], b"\0\0\0\x18ftypheic\0\0\0\0", b"\0\0\0\x1cftypavif\0\0\0\0"] {
            assert_eq!(sniff(head), Kind::Refused);
        }
        assert_eq!(sniff(b"<!doctype html>"), Kind::Other);
    }
}
