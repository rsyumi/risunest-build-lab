use risunest_ir::{Budget, stage::StageError};
use risunest_prompt::machine::{admit_selected_modules, PromptInput};
use risunest_records::{modules::{ModuleSources, SelectedModules}, preset::Preset, root::Root, settings::Settings};

pub(super) struct Evaluation {
    pub(super) input: PromptInput,
    pub(super) modules: SelectedModules,
}

impl Evaluation {
    pub(super) fn capture(input: PromptInput, budget: &Budget) -> Result<Self, StageError> {
        let settings = Settings::derive(Root::new(&input.root)?, input.preset.as_ref().map(Preset::new).transpose()?, budget)?;
        let sources = ModuleSources::for_conversation(&input.root, &input.character, &input.conversation, budget)?;
        let modules = SelectedModules::capture(&sources, settings.get("moduleIntergration", budget)?, budget)?;
        admit_selected_modules(&modules, budget)?;
        Ok(Self { input, modules })
    }
}
