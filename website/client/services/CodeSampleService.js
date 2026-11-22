import { sampleCode } from '/shared/scripts/samples.js'

/** @import LanguageService from '/services/LanguageService.js' */
@Service
@Lifetime({"value":"prototype"})
export default class CodeSampleService {
    /** @type {LanguageService} */
    @Needs
    language

    /** @type {string} */
    topic

    /** @returns {string} */
    code() {
        return sampleCode(this.language.chosen, this.topic)
    }
}
