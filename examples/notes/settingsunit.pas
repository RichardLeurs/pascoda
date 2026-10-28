unit settingsunit;

{$mode objfpc}{$H+}

interface

uses
  Classes, SysUtils, Forms, Controls, Graphics, Dialogs, StdCtrls, ExtCtrls,
  Buttons;

type
  TSettingsForm = class(TForm)
    WordWrapCheckBox: TCheckBox;
    FontSizeLabel: TLabel;
    FontSizeCombo: TComboBox;
    ThemeGroup: TRadioGroup;
    OKButton: TBitBtn;
    CancelButton: TBitBtn;
  public
    procedure LoadSettings(AWordWrap: Boolean; AFontSize: Integer);
    function WordWrapSetting: Boolean;
    function FontSizeSetting: Integer;
  end;

var
  SettingsForm: TSettingsForm;

implementation

{$R *.lfm}

const
  DefaultFontSize = 12;

// Shows the current values. Unknown font sizes fall back to DefaultFontSize.
procedure TSettingsForm.LoadSettings(AWordWrap: Boolean; AFontSize: Integer);
var
  Index: Integer;
begin
  WordWrapCheckBox.Checked := AWordWrap;
  Index := FontSizeCombo.Items.IndexOf(IntToStr(AFontSize));
  if Index < 0 then
    Index := FontSizeCombo.Items.IndexOf(IntToStr(DefaultFontSize));
  FontSizeCombo.ItemIndex := Index;
end;

function TSettingsForm.WordWrapSetting: Boolean;
begin
  Result := WordWrapCheckBox.Checked;
end;

function TSettingsForm.FontSizeSetting: Integer;
begin
  Result := StrToIntDef(FontSizeCombo.Text, DefaultFontSize);
end;

end.
