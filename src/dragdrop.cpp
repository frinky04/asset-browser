#include "dragdrop.h"
#include "util.h"
#include <ole2.h>
#include <shlobj.h>
#include <shellapi.h>

namespace {

class DropSource : public IDropSource {
    LONG refs = 1;
public:
    // IUnknown
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** ppv) override {
        if (riid == IID_IUnknown || riid == IID_IDropSource) {
            *ppv = static_cast<IDropSource*>(this);
            AddRef();
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return (ULONG)InterlockedIncrement(&refs); }
    ULONG STDMETHODCALLTYPE Release() override {
        LONG r = InterlockedDecrement(&refs);
        if (r == 0) delete this;
        return (ULONG)r;
    }
    // IDropSource
    HRESULT STDMETHODCALLTYPE QueryContinueDrag(BOOL fEscapePressed, DWORD grfKeyState) override {
        if (fEscapePressed) return DRAGDROP_S_CANCEL;
        if (!(grfKeyState & MK_LBUTTON)) return DRAGDROP_S_DROP;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GiveFeedback(DWORD) override {
        return DRAGDROP_S_USEDEFAULTCURSORS;
    }
};

class FileDataObject : public IDataObject {
    LONG refs = 1;
    HGLOBAL hDrop = nullptr; // owned CF_HDROP payload

public:
    explicit FileDataObject(const std::vector<std::wstring>& paths) {
        size_t chars = 0;
        for (auto& p : paths) chars += p.size() + 1;
        chars += 1; // double null terminator
        size_t bytes = sizeof(DROPFILES) + chars * sizeof(wchar_t);
        hDrop = GlobalAlloc(GHND, bytes);
        if (!hDrop) return;
        auto* df = (DROPFILES*)GlobalLock(hDrop);
        df->pFiles = sizeof(DROPFILES);
        df->fWide = TRUE;
        wchar_t* dst = (wchar_t*)((BYTE*)df + sizeof(DROPFILES));
        for (auto& p : paths) {
            memcpy(dst, p.c_str(), (p.size() + 1) * sizeof(wchar_t));
            dst += p.size() + 1;
        }
        // GHND zero-fills: final extra null already present
        GlobalUnlock(hDrop);
    }
    ~FileDataObject() { if (hDrop) GlobalFree(hDrop); }
    bool Valid() const { return hDrop != nullptr; }

    // IUnknown
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** ppv) override {
        if (riid == IID_IUnknown || riid == IID_IDataObject) {
            *ppv = static_cast<IDataObject*>(this);
            AddRef();
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return (ULONG)InterlockedIncrement(&refs); }
    ULONG STDMETHODCALLTYPE Release() override {
        LONG r = InterlockedDecrement(&refs);
        if (r == 0) delete this;
        return (ULONG)r;
    }

    // IDataObject
    HRESULT STDMETHODCALLTYPE GetData(FORMATETC* fmt, STGMEDIUM* med) override {
        if (QueryGetData(fmt) != S_OK) return DV_E_FORMATETC;
        // Hand out a copy so the receiver can own/free it
        SIZE_T sz = GlobalSize(hDrop);
        HGLOBAL copy = GlobalAlloc(GHND, sz);
        if (!copy) return E_OUTOFMEMORY;
        void* src = GlobalLock(hDrop);
        void* dst = GlobalLock(copy);
        memcpy(dst, src, sz);
        GlobalUnlock(copy);
        GlobalUnlock(hDrop);
        med->tymed = TYMED_HGLOBAL;
        med->hGlobal = copy;
        med->pUnkForRelease = nullptr;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetDataHere(FORMATETC*, STGMEDIUM*) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE QueryGetData(FORMATETC* fmt) override {
        if (!fmt) return E_INVALIDARG;
        if (fmt->cfFormat == CF_HDROP && (fmt->tymed & TYMED_HGLOBAL) &&
            fmt->dwAspect == DVASPECT_CONTENT)
            return S_OK;
        return DV_E_FORMATETC;
    }
    HRESULT STDMETHODCALLTYPE GetCanonicalFormatEtc(FORMATETC*, FORMATETC* out) override {
        if (out) out->ptd = nullptr;
        return E_NOTIMPL;
    }
    HRESULT STDMETHODCALLTYPE SetData(FORMATETC*, STGMEDIUM*, BOOL) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE EnumFormatEtc(DWORD dir, IEnumFORMATETC** ppenum) override {
        if (dir != DATADIR_GET) return E_NOTIMPL;
        FORMATETC fmt{ CF_HDROP, nullptr, DVASPECT_CONTENT, -1, TYMED_HGLOBAL };
        return SHCreateStdEnumFmtEtc(1, &fmt, ppenum);
    }
    HRESULT STDMETHODCALLTYPE DAdvise(FORMATETC*, DWORD, IAdviseSink*, DWORD*) override {
        return OLE_E_ADVISENOTSUPPORTED;
    }
    HRESULT STDMETHODCALLTYPE DUnadvise(DWORD) override { return OLE_E_ADVISENOTSUPPORTED; }
    HRESULT STDMETHODCALLTYPE EnumDAdvise(IEnumSTATDATA**) override { return OLE_E_ADVISENOTSUPPORTED; }
};

} // namespace

bool DoFileDrag(const std::vector<std::wstring>& paths) {
    if (paths.empty()) return false;
    auto* data = new FileDataObject(paths);
    if (!data->Valid()) { data->Release(); return false; }
    auto* source = new DropSource();

    DWORD effect = 0;
    HRESULT hr = DoDragDrop(data, source, DROPEFFECT_COPY | DROPEFFECT_LINK, &effect);

    source->Release();
    data->Release();
    return hr == DRAGDROP_S_DROP;
}
