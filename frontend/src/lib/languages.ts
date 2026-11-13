/**
 * The languages Splee writes in, in the order of LANGUAGES in backend/src/language.rs, whose tests check that every one is
 * here with its unsubscribe page text.
 */
export const LANGUAGES = [
  { code: "en", name: "English" },
  { code: "de", name: "German" },
  { code: "fr", name: "French" },
  { code: "es", name: "Spanish" },
  { code: "pt", name: "Portuguese" },
  { code: "it", name: "Italian" },
  { code: "nl", name: "Dutch" },
  { code: "sv", name: "Swedish" },
  { code: "nb", name: "Norwegian" },
  { code: "da", name: "Danish" },
  { code: "fi", name: "Finnish" },
  { code: "pl", name: "Polish" },
  { code: "cs", name: "Czech" },
  { code: "sk", name: "Slovak" },
  { code: "ro", name: "Romanian" },
  { code: "bg", name: "Bulgarian" },
  { code: "hu", name: "Hungarian" },
  { code: "el", name: "Greek" },
  { code: "tr", name: "Turkish" },
  { code: "ru", name: "Russian" },
  { code: "uk", name: "Ukrainian" },
  { code: "ar", name: "Arabic" },
  { code: "he", name: "Hebrew" },
  { code: "ja", name: "Japanese" },
  { code: "ko", name: "Korean" },
  { code: "zh", name: "Chinese (Simplified)" },
  { code: "zh-Hant", name: "Chinese (Traditional)" },
  { code: "hi", name: "Hindi" },
  { code: "id", name: "Indonesian" },
  { code: "ms", name: "Malay" },
  { code: "th", name: "Thai" },
  { code: "vi", name: "Vietnamese" },
] as const;

export type LanguageCode = (typeof LANGUAGES)[number]["code"];

/** A campaign's language: one of LANGUAGES, or "lead" for each lead's own. */
export type CampaignLanguage = LanguageCode | "lead";

/** "German" for "de". English for "" and codes from before a language was added. */
export function languageName(code: string): string {
  return LANGUAGES.find((l) => l.code === code)?.name ?? "English";
}

/** Where a lead's own language came from, as the backend's language::OWN_JOIN says. */
export const LANGUAGE_SOURCES = {
  set: "Set on the lead",
  country: "From their country",
  site: "From their company's website",
  default: "The default, with no country or website to go by",
} as const;

export type LanguageSource = keyof typeof LANGUAGE_SOURCES;

/** The public unsubscribe page in the language of the email that linked to it. {address} is the lead's email. */
export interface UnsubscribeText {
  title: string;
  willStop: string;
  button: string;
  done: string;
  thisAddress: string;
  failed: string;
}

export const UNSUBSCRIBE_TEXT: Record<LanguageCode, UnsubscribeText> = {
  en: {
    title: "Unsubscribe?",
    willStop: "{address} won't get any more emails from this sender.",
    button: "Unsubscribe",
    done: "You're unsubscribed",
    thisAddress: "This address",
    failed: "That didn't go through. Try again, or reply to the email and ask the sender to stop.",
  },
  de: {
    title: "Abmelden?",
    willStop: "{address} erhält keine weiteren E-Mails von diesem Absender.",
    button: "Abmelden",
    done: "Sie sind abgemeldet",
    thisAddress: "Diese Adresse",
    failed: "Das hat nicht geklappt. Versuchen Sie es erneut oder antworten Sie auf die E-Mail und bitten Sie den Absender, keine weiteren zu senden.",
  },
  fr: {
    title: "Se désinscrire ?",
    willStop: "{address} ne recevra plus d'e-mails de cet expéditeur.",
    button: "Se désinscrire",
    done: "Vous êtes désinscrit",
    thisAddress: "Cette adresse",
    failed: "Cela n'a pas fonctionné. Réessayez, ou répondez à l'e-mail pour demander à l'expéditeur d'arrêter.",
  },
  es: {
    title: "¿Darse de baja?",
    willStop: "{address} no recibirá más correos de este remitente.",
    button: "Darse de baja",
    done: "Se ha dado de baja",
    thisAddress: "Esta dirección",
    failed: "No se ha podido completar. Inténtelo de nuevo o responda al correo y pida al remitente que deje de escribirle.",
  },
  pt: {
    title: "Cancelar inscrição?",
    willStop: "{address} não receberá mais e-mails deste remetente.",
    button: "Cancelar inscrição",
    done: "Inscrição cancelada",
    thisAddress: "Este endereço",
    failed: "Não deu certo. Tente de novo ou responda ao e-mail pedindo ao remetente que pare.",
  },
  it: {
    title: "Annullare l'iscrizione?",
    willStop: "{address} non riceverà più email da questo mittente.",
    button: "Annulla l'iscrizione",
    done: "Iscrizione annullata",
    thisAddress: "Questo indirizzo",
    failed: "Non è andato a buon fine. Riprovi, oppure risponda all'email chiedendo al mittente di smettere.",
  },
  nl: {
    title: "Afmelden?",
    willStop: "{address} ontvangt geen e-mails meer van deze afzender.",
    button: "Afmelden",
    done: "U bent afgemeld",
    thisAddress: "Dit adres",
    failed: "Dat is niet gelukt. Probeer het opnieuw, of beantwoord de e-mail en vraag de afzender te stoppen.",
  },
  sv: {
    title: "Avregistrera dig?",
    willStop: "{address} får inga fler mejl från den här avsändaren.",
    button: "Avregistrera",
    done: "Du är avregistrerad",
    thisAddress: "Den här adressen",
    failed: "Det gick inte. Försök igen, eller svara på mejlet och be avsändaren sluta.",
  },
  nb: {
    title: "Melde deg av?",
    willStop: "{address} får ikke flere e-poster fra denne avsenderen.",
    button: "Meld av",
    done: "Du er meldt av",
    thisAddress: "Denne adressen",
    failed: "Det gikk ikke. Prøv igjen, eller svar på e-posten og be avsenderen slutte.",
  },
  da: {
    title: "Afmeld?",
    willStop: "{address} får ikke flere e-mails fra denne afsender.",
    button: "Afmeld",
    done: "Du er afmeldt",
    thisAddress: "Denne adresse",
    failed: "Det lykkedes ikke. Prøv igen, eller svar på e-mailen og bed afsenderen om at stoppe.",
  },
  fi: {
    title: "Peru tilaus?",
    willStop: "{address} ei saa enää sähköposteja tältä lähettäjältä.",
    button: "Peru tilaus",
    done: "Tilaus on peruttu",
    thisAddress: "Tämä osoite",
    failed: "Se ei onnistunut. Yritä uudelleen tai vastaa sähköpostiin ja pyydä lähettäjää lopettamaan.",
  },
  pl: {
    title: "Wypisać się?",
    willStop: "{address} nie otrzyma więcej wiadomości od tego nadawcy.",
    button: "Wypisz się",
    done: "Wypisano",
    thisAddress: "Ten adres",
    failed: "Nie udało się. Spróbuj ponownie lub odpowiedz na wiadomość i poproś nadawcę o zaprzestanie.",
  },
  cs: {
    title: "Odhlásit se?",
    willStop: "Na {address} už od tohoto odesílatele nepřijdou žádné e-maily.",
    button: "Odhlásit se",
    done: "Jste odhlášeni",
    thisAddress: "tuto adresu",
    failed: "Nepodařilo se. Zkuste to znovu, nebo na e-mail odpovězte a požádejte odesílatele, aby přestal.",
  },
  sk: {
    title: "Odhlásiť sa?",
    willStop: "Na {address} už od tohto odosielateľa neprídu žiadne e-maily.",
    button: "Odhlásiť sa",
    done: "Ste odhlásení",
    thisAddress: "túto adresu",
    failed: "Nepodarilo sa. Skúste to znova alebo odpovedzte na e-mail a požiadajte odosielateľa, aby prestal.",
  },
  ro: {
    title: "Vă dezabonați?",
    willStop: "{address} nu va mai primi e-mailuri de la acest expeditor.",
    button: "Dezabonare",
    done: "V-ați dezabonat",
    thisAddress: "Această adresă",
    failed: "Nu a funcționat. Încercați din nou sau răspundeți la e-mail și cereți expeditorului să se oprească.",
  },
  bg: {
    title: "Отписване?",
    willStop: "{address} няма да получава повече имейли от този подател.",
    button: "Отписване",
    done: "Отписахте се",
    thisAddress: "Този адрес",
    failed: "Не се получи. Опитайте отново или отговорете на имейла и помолете подателя да спре.",
  },
  hu: {
    title: "Leiratkozik?",
    willStop: "A(z) {address} címre nem érkezik több e-mail ettől a feladótól.",
    button: "Leiratkozás",
    done: "Sikeresen leiratkozott",
    thisAddress: "ez a cím",
    failed: "Ez nem sikerült. Próbálja újra, vagy válaszoljon az e-mailre, és kérje a feladót, hogy ne írjon többet.",
  },
  el: {
    title: "Διαγραφή;",
    willStop: "Η διεύθυνση {address} δεν θα λαμβάνει άλλα email από αυτόν τον αποστολέα.",
    button: "Διαγραφή",
    done: "Η διαγραφή ολοκληρώθηκε",
    thisAddress: "αυτή",
    failed: "Δεν ολοκληρώθηκε. Δοκιμάστε ξανά ή απαντήστε στο email και ζητήστε από τον αποστολέα να σταματήσει.",
  },
  tr: {
    title: "Abonelikten çıkılsın mı?",
    willStop: "{address} bu göndericiden artık e-posta almayacak.",
    button: "Abonelikten çık",
    done: "Abonelikten çıktınız",
    thisAddress: "Bu adres",
    failed: "Bu işlem tamamlanmadı. Tekrar deneyin ya da e-postayı yanıtlayıp göndericiden durmasını isteyin.",
  },
  ru: {
    title: "Отписаться?",
    willStop: "На {address} больше не будут приходить письма от этого отправителя.",
    button: "Отписаться",
    done: "Вы отписались",
    thisAddress: "этот адрес",
    failed: "Не получилось. Попробуйте ещё раз или ответьте на письмо и попросите отправителя больше не писать.",
  },
  uk: {
    title: "Відписатися?",
    willStop: "На {address} більше не надходитимуть листи від цього відправника.",
    button: "Відписатися",
    done: "Ви відписалися",
    thisAddress: "цю адресу",
    failed: "Не вдалося. Спробуйте ще раз або дайте відповідь на лист і попросіть відправника більше не писати.",
  },
  ar: {
    title: "إلغاء الاشتراك؟",
    willStop: "لن يتلقى {address} أي رسائل أخرى من هذا المرسل.",
    button: "إلغاء الاشتراك",
    done: "تم إلغاء اشتراكك",
    thisAddress: "هذا العنوان",
    failed: "لم تتم العملية. حاول مرة أخرى، أو رد على الرسالة واطلب من المرسل التوقف.",
  },
  he: {
    title: "להסיר מרשימת התפוצה?",
    willStop: "{address} לא יקבל עוד הודעות מהשולח הזה.",
    button: "הסרה",
    done: "הוסרת מהרשימה",
    thisAddress: "הכתובת הזו",
    failed: "זה לא הצליח. נסו שוב, או השיבו למייל ובקשו מהשולח להפסיק.",
  },
  ja: {
    title: "配信を停止しますか？",
    willStop: "{address} には今後この送信者からメールが届きません。",
    button: "配信を停止する",
    done: "配信を停止しました",
    thisAddress: "このアドレス",
    failed: "処理できませんでした。もう一度お試しいただくか、メールに返信して送信者に停止を依頼してください。",
  },
  ko: {
    title: "수신을 거부하시겠습니까?",
    willStop: "{address}(으)로 이 발신자의 이메일이 더 이상 발송되지 않습니다.",
    button: "수신 거부",
    done: "수신 거부되었습니다",
    thisAddress: "이 주소",
    failed: "처리되지 않았습니다. 다시 시도하시거나 이메일에 답장하여 발신자에게 중단을 요청하세요.",
  },
  zh: {
    title: "退订？",
    willStop: "{address} 将不再收到该发件人的邮件。",
    button: "退订",
    done: "您已退订",
    thisAddress: "此邮箱",
    failed: "操作未成功。请重试，或回复邮件请发件人停止发送。",
  },
  "zh-Hant": {
    title: "取消訂閱？",
    willStop: "{address} 將不再收到此寄件者的郵件。",
    button: "取消訂閱",
    done: "您已取消訂閱",
    thisAddress: "此電子郵件地址",
    failed: "操作未成功。請重試，或回覆郵件請寄件者停止寄送。",
  },
  hi: {
    title: "सदस्यता छोड़ें?",
    willStop: "{address} को अब इस प्रेषक से कोई ईमेल नहीं मिलेगा।",
    button: "सदस्यता छोड़ें",
    done: "आपने सदस्यता छोड़ दी है",
    thisAddress: "इस पते",
    failed: "यह पूरा नहीं हो सका। फिर से कोशिश करें, या ईमेल का जवाब देकर प्रेषक से भेजना बंद करने को कहें।",
  },
  id: {
    title: "Berhenti berlangganan?",
    willStop: "{address} tidak akan menerima email lagi dari pengirim ini.",
    button: "Berhenti berlangganan",
    done: "Anda telah berhenti berlangganan",
    thisAddress: "Alamat ini",
    failed: "Gagal diproses. Coba lagi, atau balas email tersebut dan minta pengirim untuk berhenti.",
  },
  ms: {
    title: "Berhenti melanggan?",
    willStop: "{address} tidak akan menerima e-mel lagi daripada pengirim ini.",
    button: "Berhenti melanggan",
    done: "Anda telah berhenti melanggan",
    thisAddress: "Alamat ini",
    failed: "Tidak berjaya. Cuba lagi, atau balas e-mel itu dan minta pengirim berhenti.",
  },
  th: {
    title: "ยกเลิกการรับอีเมล?",
    willStop: "{address} จะไม่ได้รับอีเมลจากผู้ส่งรายนี้อีก",
    button: "ยกเลิกการรับอีเมล",
    done: "ยกเลิกการรับอีเมลแล้ว",
    thisAddress: "ที่อยู่นี้",
    failed: "ดำเนินการไม่สำเร็จ ลองอีกครั้ง หรือตอบกลับอีเมลเพื่อขอให้ผู้ส่งหยุดส่ง",
  },
  vi: {
    title: "Hủy đăng ký?",
    willStop: "{address} sẽ không nhận thêm email nào từ người gửi này.",
    button: "Hủy đăng ký",
    done: "Bạn đã hủy đăng ký",
    thisAddress: "Địa chỉ này",
    failed: "Không thực hiện được. Hãy thử lại, hoặc trả lời email và đề nghị người gửi dừng lại.",
  },
};

/** Right-to-left scripts, so the page lays out the right way round. */
export const RTL: readonly string[] = ["ar", "he"];
